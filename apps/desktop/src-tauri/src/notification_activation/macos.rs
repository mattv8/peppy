use super::owner::Signal;
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

type Signals = Arc<Mutex<HashMap<String, Signal>>>;

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
            _notification: &UNNotification,
            completion: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion
                .call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List,));
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
    pub fn new(_app: &tauri::AppHandle, signals: Signals) -> Option<Self> {
        Some(Self {
            signals,
            in_flight: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn post(&mut self, id: &str, title: &str, body: &str, signal: Signal) -> Result<Item, ()> {
        // UN owns completion blocks until posting finishes. Removing a request
        // does not guarantee its completion has run, so bound this lifetime too.
        reserve_completion(&self.in_flight)?;
        objc2::rc::autoreleasepool(|_| {
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(title));
            content.setBody(&NSString::from_str(body));
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
                if !error.is_null() {
                    eprintln!("peppy: asynchronous native notification post failed");
                    signal.retire();
                }
                // A request may finish posting after cap/session cleanup. Remove it
                // again rather than leaving an OS item whose callback was retired.
                completing.complete(remove_os);
                let _ = accepted.try_send(if error.is_null() { Ok(()) } else { Err(()) });
            });
            UNUserNotificationCenter::currentNotificationCenter()
                .addNotificationRequest_withCompletionHandler(&request, Some(&completion));
            if super::owner::await_platform_acceptance(&acceptance, super::owner::PLATFORM_TIMEOUT)
                .is_err()
            {
                // Timeout is not acceptance. Fence/remove the request now; a late
                // successful completion repeats cleanup and cannot ACK the candidate.
                self.remove(&item);
                return Err(());
            }
            Ok(item)
        })
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
}
