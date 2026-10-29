use super::{owner::Signal, PostError};
use block2::{DynBlock, RcBlock};
use objc2::{
    define_class, msg_send, rc::Retained, runtime::ProtocolObject, AnyThread, DefinedClass,
};
use objc2_foundation::{NSArray, NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::*;
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tauri::Emitter;

type Signals = Arc<Mutex<HashMap<String, Signal>>>;
const UN_ERROR_CODE_NOTIFICATIONS_NOT_ALLOWED: isize = 1;

fn notification_content(
    title: &str,
    body: &str,
    sound: bool,
) -> Retained<UNMutableNotificationContent> {
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    if sound {
        content.setSound(Some(&UNNotificationSound::defaultSound()));
    }
    content
}

fn presentation_options(sound: bool) -> UNNotificationPresentationOptions {
    let options =
        UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List;
    if sound {
        options | UNNotificationPresentationOptions::Sound
    } else {
        options
    }
}

define_class!(
    // NSObject has no additional subclass requirements. Ivars are thread-safe;
    // UN invokes these methods on its own callback queue, not necessarily main.
    #[unsafe(super = NSObject)]
    #[ivars = Signals]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn response(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &DynBlock<dyn Fn()>,
        ) {
            let id = response.notification().request().identifier().to_string();
            let signal = self
                .ivars()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .get(&id)
                .cloned();
            if let Some(signal) = signal {
                if unsafe {
                    response
                        .actionIdentifier()
                        .isEqualToString(UNNotificationDefaultActionIdentifier)
                } {
                    signal.click();
                } else if unsafe {
                    response
                        .actionIdentifier()
                        .isEqualToString(UNNotificationDismissActionIdentifier)
                } {
                    signal.retire();
                }
            }
            completion.call(());
        }

        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn present(
            &self,
            _center: &UNUserNotificationCenter,
            notification: &UNNotification,
            completion: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            let sound = notification.request().content().sound().is_some();
            completion.call((presentation_options(sound),));
        }
    }
);

thread_local! {
    // UN's delegate property is weak. Keep this strong reference on the setup
    // main thread for process lifetime (including in-flight completion calls).
    static DELEGATE: RefCell<Option<Retained<Delegate>>> = const { RefCell::new(None) };
}

pub fn prepare() -> Option<Signals> {
    NSBundle::mainBundle().bundleIdentifier()?;
    let signals = Signals::default();
    let delegate: Retained<Delegate> =
        unsafe { msg_send![super(Delegate::alloc().set_ivars(signals.clone())), init] };
    UNUserNotificationCenter::currentNotificationCenter()
        .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    DELEGATE.with(|slot| *slot.borrow_mut() = Some(delegate));
    Some(signals)
}

pub struct Backend {
    signals: Signals,
    in_flight: Arc<AtomicUsize>,
    requested: bool,
    app: tauri::AppHandle,
}
#[derive(Clone)]
pub struct Item {
    id: String,
    retiring: Arc<AtomicBool>,
}

impl Item {
    fn complete(&self, remove: impl FnOnce(&str)) {
        if self.retiring.load(Ordering::Acquire) {
            remove(&self.id);
        }
    }

    fn retire(&self, remove: impl FnOnce(&str) -> bool, release: impl FnOnce(&str)) -> bool {
        // Completion can arrive between OS removal and callback release. Publish
        // retirement first, and keep it set across cleanup failure/retry.
        self.retiring.store(true, Ordering::Release);
        if !remove(&self.id) {
            return false;
        }
        release(&self.id);
        true
    }
}

impl Backend {
    pub fn new(app: &tauri::AppHandle, signals: Signals) -> Option<Self> {
        Some(Self {
            signals,
            in_flight: Arc::new(AtomicUsize::new(0)),
            requested: false,
            app: app.clone(),
        })
    }

    pub fn post(
        &mut self,
        id: &str,
        title: &str,
        body: &str,
        sound: bool,
        signal: Signal,
    ) -> Result<Item, PostError> {
        // UN owns completion blocks until posting finishes. Removing a request
        // does not guarantee its completion has run, so bound this lifetime too.
        reserve_completion(&self.in_flight).map_err(|_| PostError::Failed)?;
        objc2::rc::autoreleasepool(|_| {
            let content = notification_content(title, body, sound);
            let identifier = NSString::from_str(id);
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                &identifier,
                &content,
                None,
            );
            self.signals
                .lock()
                .unwrap()
                .insert(id.into(), signal.clone());
            let item = Item {
                id: id.to_owned(),
                retiring: Arc::new(AtomicBool::new(false)),
            };
            let completing = item.clone();
            let in_flight = self.in_flight.clone();
            let (accepted, acceptance) = std::sync::mpsc::sync_channel(1);
            let completion = RcBlock::new(move |error: *mut NSError| {
                in_flight.fetch_sub(1, Ordering::AcqRel);
                let result = if error.is_null() {
                    Ok(())
                } else {
                    eprintln!("peppy: asynchronous native notification post failed");
                    signal.retire();
                    // SAFETY: the completion handler supplies a valid NSError when non-null.
                    // UNErrorCodeNotificationsNotAllowed is 1.
                    Err(
                        if unsafe { (*error).code() } == UN_ERROR_CODE_NOTIFICATIONS_NOT_ALLOWED {
                            PostError::NotAllowed
                        } else {
                            PostError::Failed
                        },
                    )
                };
                // A request may finish posting after cap/session cleanup. Remove it
                // again rather than leaving an OS item whose callback was retired.
                completing.complete(remove_os);
                let _ = accepted.try_send(result);
            });
            UNUserNotificationCenter::currentNotificationCenter()
                .addNotificationRequest_withCompletionHandler(&request, Some(&completion));
            let result = super::owner::await_platform_acceptance(
                &acceptance,
                super::owner::PLATFORM_TIMEOUT,
            )
            .map_err(|_| PostError::Failed)?;
            if let Err(error) = result {
                // Timeout is not acceptance. Fence/remove the request now; a late
                // successful completion repeats cleanup and cannot ACK the candidate.
                self.remove(&item);
                return Err(error);
            }
            Ok(item)
        })
    }

    pub fn ensure_authorized(&mut self) -> Result<(), PostError> {
        let app = self.app.clone();
        authorize(
            &mut self.requested,
            crate::notification_permission::macos::authorization_status,
            move || {
                let completed = app.clone();
                crate::notification_permission::macos::request_authorization(move || {
                    let _ = completed.emit(crate::STATE_EVENT, ());
                })
            },
        )
    }

    pub fn remove(&mut self, item: &Item) -> bool {
        item.retire(
            |id| {
                remove_os(id);
                true // UN's removal methods have no error result.
            },
            |id| {
                self.signals.lock().unwrap().remove(id);
            },
        )
    }
}

fn authorize(
    requested: &mut bool,
    mut read: impl FnMut() -> Option<crate::notification_permission::MacAuth>,
    mut request: impl FnMut(),
) -> Result<(), PostError> {
    match read() {
        Some(crate::notification_permission::MacAuth::Denied) => Err(PostError::NotAllowed),
        Some(crate::notification_permission::MacAuth::NotDetermined) if !*requested => {
            *requested = true;
            request();
            Err(PostError::AwaitingAuthorization)
        }
        Some(crate::notification_permission::MacAuth::NotDetermined) => {
            Err(PostError::AwaitingAuthorization)
        }
        Some(_) | None => Ok(()),
    }
}

fn remove_os(id: &str) {
    objc2::rc::autoreleasepool(|_| {
        let ids = NSArray::from_retained_slice(&[NSString::from_str(id)]);
        let center = UNUserNotificationCenter::currentNotificationCenter();
        center.removePendingNotificationRequestsWithIdentifiers(&ids);
        center.removeDeliveredNotificationsWithIdentifiers(&ids);
    });
}

fn reserve_completion(in_flight: &AtomicUsize) -> Result<(), ()> {
    in_flight
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < super::owner::CAPACITY).then_some(count + 1)
        })
        .map(|_| ())
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_sound_macos_content_and_foreground_policy_match() {
        for enabled in [false, true] {
            let content = notification_content("title", "body", enabled);
            assert_eq!(content.sound().is_some(), enabled);
            assert_eq!(
                presentation_options(enabled).contains(UNNotificationPresentationOptions::Sound),
                enabled
            );
            assert!(
                presentation_options(enabled).contains(UNNotificationPresentationOptions::Banner)
            );
        }
    }
    use std::cell::Cell;

    #[test]
    fn notification_mac_post_completion_between_os_removal_and_callback_release_is_cleaned() {
        let item = Item {
            id: "request".into(),
            retiring: Arc::new(AtomicBool::new(false)),
        };
        let os_item = Cell::new(true);
        let callback_owned = Cell::new(true);
        let removals = Cell::new(0);
        assert!(item.retire(
            |_| {
                os_item.set(false);
                removals.set(removals.get() + 1);
                // Deterministically complete the unfinished post after the first
                // OS removal, while the callback is still registered.
                os_item.set(true);
                item.complete(|_| {
                    assert!(callback_owned.get());
                    os_item.set(false);
                    removals.set(removals.get() + 1);
                });
                true
            },
            |_| {
                assert!(!os_item.get(), "late post escaped retirement cleanup");
                callback_owned.set(false);
            },
        ));
        assert_eq!(removals.get(), 2);
        assert!(!os_item.get());
        assert!(!callback_owned.get());
    }

    #[test]
    fn notification_mac_failed_retirement_keeps_callback_and_fence_for_retry() {
        let item = Item {
            id: "request".into(),
            retiring: Arc::new(AtomicBool::new(false)),
        };
        let callback_owned = Cell::new(true);
        assert!(!item.retire(|_| false, |_| callback_owned.set(false)));
        assert!(callback_owned.get());
        let completion_cleaned = Cell::new(false);
        item.complete(|_| completion_cleaned.set(true));
        assert!(completion_cleaned.get());
        assert!(callback_owned.get());
        assert!(item.retire(
            |_| {
                assert!(callback_owned.get());
                true
            },
            |_| callback_owned.set(false)
        ));
        assert!(!callback_owned.get());
    }

    #[test]
    fn notification_unfinished_mac_posts_are_bounded_even_after_registry_retirement() {
        let in_flight = AtomicUsize::new(0);
        for _ in 0..256 {
            reserve_completion(&in_flight).unwrap();
        }
        assert!(reserve_completion(&in_flight).is_err());
        in_flight.fetch_sub(1, Ordering::AcqRel);
        assert!(reserve_completion(&in_flight).is_ok());
        assert!(reserve_completion(&in_flight).is_err());
    }

    #[test]
    fn notification_authorization_holds_not_determined_once_until_completion() {
        let mut requested = false;
        let request_count = Cell::new(0);
        for _ in 0..2 {
            assert_eq!(
                authorize(
                    &mut requested,
                    || Some(crate::notification_permission::MacAuth::NotDetermined),
                    || request_count.set(request_count.get() + 1),
                ),
                Err(PostError::AwaitingAuthorization)
            );
        }
        assert_eq!(request_count.get(), 1);
    }

    #[test]
    fn notification_authorization_denial_does_not_request() {
        let mut requested = false;
        let request_count = Cell::new(0);
        assert_eq!(
            authorize(
                &mut requested,
                || Some(crate::notification_permission::MacAuth::Denied),
                || request_count.set(request_count.get() + 1),
            ),
            Err(PostError::NotAllowed)
        );
        assert!(!requested);
        assert_eq!(request_count.get(), 0);
    }

    #[test]
    fn notification_authorization_granted_proceeds_without_request() {
        let mut requested = false;
        let request_count = Cell::new(0);
        assert_eq!(
            authorize(
                &mut requested,
                || Some(crate::notification_permission::MacAuth::Authorized),
                || request_count.set(request_count.get() + 1),
            ),
            Ok(())
        );
        assert_eq!(request_count.get(), 0);
    }
}
