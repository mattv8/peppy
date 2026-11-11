use super::{owner::Signal, ActionAvailability, PostError};
use crate::notification_icon::{self, Icon, Lease};
use block2::{DynBlock, RcBlock};
use objc2::{
    define_class, msg_send, rc::Retained, runtime::ProtocolObject, AnyThread, DefinedClass,
};
use objc2_foundation::{
    NSArray, NSBundle, NSDictionary, NSError, NSObject, NSObjectProtocol, NSProcessInfo, NSSet,
    NSString, NSURL,
};
use objc2_user_notifications::*;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tauri::Emitter;

type Signals = Arc<Mutex<HashMap<(String, String), Signal>>>;
type Operations = Arc<Mutex<()>>;
type PendingIds = Arc<Mutex<PendingIdsState>>;
const UN_ERROR_CODE_NOTIFICATIONS_NOT_ALLOWED: isize = 1;
const PRESENTATION_TOKEN_KEY: &str = "peppy.presentation-token";
/// Registered once in `prepare`; posts opt in by category, and the action id is the only payload.
const COPY_CATEGORY: &str = "peppy.one-time-code";
const COPY_ACTION: &str = "copy-code";
const REVEAL_ACTION: &str = "reveal";
const REPLY_ACTION: &str = "reply";
const REVEAL_CATEGORY: &str = "peppy.quick-reveal";
const REPLY_CATEGORY: &str = "peppy.quick-reply";
const COPY_REPLY_CATEGORY: &str = "peppy.quick-copy-reply";

fn notification_content(
    title: &str,
    body: &str,
    sound: bool,
    icon: Option<&Lease>,
) -> Retained<UNMutableNotificationContent> {
    let content = UNMutableNotificationContent::new();
    let token = NSString::from_str(&uuid::Uuid::new_v4().simple().to_string());
    let key = NSString::from_str(PRESENTATION_TOKEN_KEY);
    let user_info = NSDictionary::from_retained_objects(&[&*key], &[token]);
    // SAFETY: UNMutableNotificationContent copies a dictionary of property-list
    // values; both this key and its opaque token are NSString instances.
    let _: () = unsafe { msg_send![&content, setUserInfo: &*user_info] };
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    if sound {
        content.setSound(Some(&UNNotificationSound::defaultSound()));
    }
    if let Some(icon) = icon {
        let path = NSString::from_str(&icon.path().to_string_lossy());
        let url = NSURL::fileURLWithPath(&path);
        if let Ok(attachment) = unsafe {
            UNNotificationAttachment::attachmentWithIdentifier_URL_options_error(
                &NSString::from_str("content-icon"),
                &url,
                None,
            )
        } {
            let attachments = NSArray::from_retained_slice(&[attachment]);
            content.setAttachments(&attachments);
        }
    }
    content
}

fn callback_key(request: &UNNotificationRequest) -> Option<(String, String)> {
    let token = request
        .content()
        .userInfo()
        .objectForKey(&NSString::from_str(PRESENTATION_TOKEN_KEY))?
        .downcast_ref::<NSString>()?
        .to_string();
    (uuid::Uuid::parse_str(&token).ok()?.simple().to_string() == token)
        .then(|| (request.identifier().to_string(), token))
}

fn registered_callback<'a, T>(
    callbacks: &'a HashMap<(String, String), T>,
    request: &UNNotificationRequest,
) -> Option<&'a T> {
    callback_key(request).and_then(|key| callbacks.get(&key))
}

fn notification_request(
    id: &str,
    content: &UNMutableNotificationContent,
) -> Retained<UNNotificationRequest> {
    UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(id),
        content,
        None,
    )
}

#[derive(Default)]
struct PendingIdsState {
    active: HashSet<String>,
    quarantined: HashSet<String>,
}

fn reserve_pending_id(
    in_flight: &AtomicUsize,
    pending_ids: &PendingIds,
    id: &str,
) -> Result<(), ()> {
    reserve_completion(in_flight)?;
    let mut pending = pending_ids.lock().unwrap();
    if pending.active.len() + pending.quarantined.len() < super::owner::CAPACITY
        && !pending.quarantined.contains(id)
        && pending.active.insert(id.into())
    {
        Ok(())
    } else {
        in_flight.fetch_sub(1, Ordering::AcqRel);
        Err(())
    }
}

fn release_pending_id(in_flight: &AtomicUsize, pending_ids: &PendingIds, id: &str) {
    pending_ids.lock().unwrap().active.remove(id);
    in_flight.fetch_sub(1, Ordering::AcqRel);
}

fn quarantine_pending_id(pending_ids: &PendingIds, id: &str) {
    // Removing a native request is asynchronous, not an ordering barrier for a
    // timed-out add. Never reuse this ID during the owner's lifetime, even when
    // that add eventually completes; admission bounds these retained fences.
    pending_ids.lock().unwrap().quarantined.insert(id.into());
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
            let signal = {
                let signals = self
                    .ivars()
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                registered_callback(&signals, &response.notification().request()).cloned()
            };
            if let Some(signal) = signal {
                if unsafe {
                    response
                        .actionIdentifier()
                        .isEqualToString(UNNotificationDefaultActionIdentifier)
                } {
                    signal.click();
                } else if response
                    .actionIdentifier()
                    .isEqualToString(&NSString::from_str(COPY_ACTION))
                {
                    signal.copy();
                } else if response
                    .actionIdentifier()
                    .isEqualToString(&NSString::from_str(REVEAL_ACTION))
                {
                    signal.reveal();
                } else if response
                    .actionIdentifier()
                    .isEqualToString(&NSString::from_str(REPLY_ACTION))
                {
                    // A forged/malformed action must not reinterpret a normal
                    // response as text input.
                    if let Some(input) = response.downcast_ref::<UNTextInputNotificationResponse>()
                    {
                        signal.reply(input.userText().to_string());
                    }
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
    let center = UNUserNotificationCenter::currentNotificationCenter();
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    register_categories(&center);
    DELEGATE.with(|slot| *slot.borrow_mut() = Some(delegate));
    Some(signals)
}

/// The Copy action is a background action (no foreground option), so it never brings Peppy forward.
fn register_categories(center: &UNUserNotificationCenter) {
    let copy_action = action(COPY_ACTION, "Copy code", "doc.on.doc");
    let actions = NSArray::from_retained_slice(std::slice::from_ref(&copy_action));
    let intents: Retained<NSArray<NSString>> = NSArray::new();
    let copy_category =
        UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
            &NSString::from_str(COPY_CATEGORY),
            &actions,
            &intents,
            UNNotificationCategoryOptionNone,
        );
    let reveal = category(
        REVEAL_CATEGORY,
        vec![action(REVEAL_ACTION, "Show message", "chevron.down")],
        &intents,
    );
    let reply = category(REPLY_CATEGORY, vec![reply_action().into_super()], &intents);
    let copy_reply = category(
        COPY_REPLY_CATEGORY,
        vec![copy_action, reply_action().into_super()],
        &intents,
    );
    center.setNotificationCategories(&NSSet::from_retained_slice(&[
        copy_category,
        reveal,
        reply,
        copy_reply,
    ]));
}

fn category(
    identifier: &str,
    actions: Vec<Retained<UNNotificationAction>>,
    intents: &NSArray<NSString>,
) -> Retained<UNNotificationCategory> {
    let actions = NSArray::from_retained_slice(&actions);
    UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
        &NSString::from_str(identifier),
        &actions,
        intents,
        UNNotificationCategoryOptionNone,
    )
}

fn category_for(actions: ActionAvailability) -> Option<&'static str> {
    match (actions.copy, actions.reveal, actions.reply) {
        (_, true, false) => Some(REVEAL_CATEGORY),
        (true, false, true) => Some(COPY_REPLY_CATEGORY),
        (false, false, true) => Some(REPLY_CATEGORY),
        _ => None,
    }
}

fn action(identifier: &str, title: &str, symbol: &str) -> Retained<UNNotificationAction> {
    let identifier = NSString::from_str(identifier);
    let title = NSString::from_str(title);
    if macos12_or_later() {
        let icon = UNNotificationActionIcon::iconWithSystemImageName(&NSString::from_str(symbol));
        UNNotificationAction::actionWithIdentifier_title_options_icon(
            &identifier,
            &title,
            UNNotificationActionOptionNone,
            Some(&icon),
        )
    } else {
        UNNotificationAction::actionWithIdentifier_title_options(
            &identifier,
            &title,
            UNNotificationActionOptionNone,
        )
    }
}

fn reply_action() -> Retained<UNTextInputNotificationAction> {
    let identifier = NSString::from_str(REPLY_ACTION);
    let title = NSString::from_str("Reply");
    let send = NSString::from_str("Send");
    let placeholder = NSString::from_str("Type a reply");
    if macos12_or_later() {
        let icon =
            UNNotificationActionIcon::iconWithSystemImageName(&NSString::from_str("arrow.up"));
        UNTextInputNotificationAction::actionWithIdentifier_title_options_icon_textInputButtonTitle_textInputPlaceholder(
            &identifier,
            &title,
            UNNotificationActionOptions::AuthenticationRequired,
            Some(&icon),
            &send,
            &placeholder,
        )
    } else {
        UNTextInputNotificationAction::actionWithIdentifier_title_options_textInputButtonTitle_textInputPlaceholder(
            &identifier,
            &title,
            UNNotificationActionOptions::AuthenticationRequired,
            &send,
            &placeholder,
        )
    }
}

fn macos12_or_later() -> bool {
    NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        >= 12
}

pub struct Backend {
    signals: Signals,
    operations: Operations,
    pending_ids: PendingIds,
    in_flight: Arc<AtomicUsize>,
    requested: bool,
    app: tauri::AppHandle,
}
#[derive(Clone)]
pub struct Item {
    id: String,
    token: String,
    remove_on_completion: Arc<AtomicBool>,
    operations: Operations,
    _icon: Option<Lease>,
}

impl Item {
    fn complete(&self, remove: impl FnOnce(&str)) {
        let _operations = self.operations.lock().unwrap();
        if self.remove_on_completion.load(Ordering::Acquire) {
            remove(&self.id);
        }
    }

    fn retire(&self, remove: impl FnOnce(&str) -> bool, release: impl FnOnce(&str)) -> bool {
        let _operations = self.operations.lock().unwrap();
        // Completion can arrive between OS removal and callback release. Publish
        // cleanup first, and keep it set across cleanup failure/retry.
        self.remove_on_completion.store(true, Ordering::Release);
        if !remove(&self.id) {
            return false;
        }
        release(&self.id);
        true
    }

    fn retire_replaced<T>(&self, callbacks: &mut HashMap<(String, String), T>) {
        callbacks.remove(&(self.id.clone(), self.token.clone()));
    }
}

impl Backend {
    pub fn new(app: &tauri::AppHandle, signals: Signals) -> Option<Self> {
        Some(Self {
            signals,
            operations: Operations::default(),
            pending_ids: PendingIds::default(),
            in_flight: Arc::new(AtomicUsize::new(0)),
            requested: false,
            app: app.clone(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn post(
        &mut self,
        id: &str,
        _presentation_generation: u64,
        title: &str,
        body: &str,
        sound: bool,
        icon: Option<&Icon>,
        actions: ActionAvailability,
        signal: Signal,
    ) -> Result<Item, PostError> {
        // UN owns completion blocks until posting finishes. Removing a request
        // does not guarantee its completion has run, so bound this lifetime too.
        reserve_pending_id(&self.in_flight, &self.pending_ids, id)
            .map_err(|_| PostError::Failed)?;
        objc2::rc::autoreleasepool(|_| {
            let icon =
                content_icon(icon).and_then(|icon| notification_icon::materialize(&self.app, icon));
            let content = notification_content(title, body, sound, icon.as_ref());
            if let Some(category) = category_for(actions) {
                content.setCategoryIdentifier(&NSString::from_str(category));
            } else if actions.copy {
                content.setCategoryIdentifier(&NSString::from_str(COPY_CATEGORY));
            }
            let request = notification_request(id, &content);
            let token = callback_key(&request).expect("new content has a token");
            self.signals
                .lock()
                .unwrap()
                .insert(token.clone(), signal.clone());
            let item = Item {
                id: id.into(),
                token: token.1,
                remove_on_completion: Arc::new(AtomicBool::new(false)),
                operations: self.operations.clone(),
                _icon: icon,
            };
            let completing = item.clone();
            let in_flight = self.in_flight.clone();
            let pending_ids = self.pending_ids.clone();
            let (accepted, acceptance) = std::sync::mpsc::sync_channel(1);
            let completion = RcBlock::new(move |error: *mut NSError| {
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
                release_pending_id(&in_flight, &pending_ids, &completing.id);
                let _ = accepted.try_send(result);
            });
            {
                let _operations = self.operations.lock().unwrap();
                UNUserNotificationCenter::currentNotificationCenter()
                    .addNotificationRequest_withCompletionHandler(&request, Some(&completion));
            }
            let acceptance = super::owner::await_platform_acceptance(
                &acceptance,
                super::owner::PLATFORM_TIMEOUT,
            );
            if acceptance.is_err() {
                quarantine_pending_id(&self.pending_ids, &item.id);
            }
            platform_acceptance_outcome(acceptance, &item, |item| {
                self.remove(item);
            })?;
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
            |_| {
                self.signals
                    .lock()
                    .unwrap()
                    .remove(&(item.id.clone(), item.token.clone()));
            },
        )
    }

    pub fn retire_replaced(&mut self, item: &Item) -> bool {
        item.retire_replaced(&mut self.signals.lock().unwrap());
        true
    }
}

fn platform_acceptance_outcome(
    result: Result<Result<(), PostError>, ()>,
    item: &Item,
    retire: impl FnOnce(&Item),
) -> Result<(), PostError> {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            retire(item);
            Err(error)
        }
        Err(()) => {
            retire(item);
            Err(PostError::Failed)
        }
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

fn content_icon(icon: Option<&Icon>) -> Option<&Icon> {
    match icon {
        Some(Icon::App(_)) => icon,
        Some(Icon::Message) | None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_sound_macos_content_and_foreground_policy_match() {
        for enabled in [false, true] {
            let content = notification_content("title", "body", enabled, None);
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

    #[test]
    fn notification_macos_content_carries_its_opaque_callback_token() {
        let content = notification_content("title", "body", false, None);
        let token = content
            .userInfo()
            .objectForKey(&NSString::from_str("peppy.presentation-token"))
            .and_then(|value| {
                value
                    .downcast_ref::<NSString>()
                    .map(|token| token.to_string())
            });

        assert!(token.is_some());
    }

    #[test]
    fn notification_macos_replacements_share_native_id_but_not_callback_token() {
        let first = notification_request(
            "logical-id",
            &notification_content("title", "body", false, None),
        );
        let second = notification_request(
            "logical-id",
            &notification_content("title", "body", false, None),
        );

        assert_eq!(first.identifier().to_string(), "logical-id");
        assert_eq!(second.identifier().to_string(), "logical-id");
        assert_ne!(
            callback_key(&first).unwrap().1,
            callback_key(&second).unwrap().1
        );
    }

    #[test]
    fn notification_macos_callback_lookup_rejects_missing_malformed_and_wrong_id_tokens() {
        let valid_content = notification_content("title", "body", false, None);
        let valid = notification_request("current", &valid_content);
        let mut callbacks = HashMap::new();
        callbacks.insert(callback_key(&valid).unwrap(), true);
        assert_eq!(registered_callback(&callbacks, &valid), Some(&true));

        let missing = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str("current"),
            &UNMutableNotificationContent::new(),
            None,
        );
        assert!(registered_callback(&callbacks, &missing).is_none());

        let malformed_content = UNMutableNotificationContent::new();
        let key = NSString::from_str(PRESENTATION_TOKEN_KEY);
        let malformed = NSString::from_str("not-a-token");
        let user_info = NSDictionary::from_retained_objects(&[&*key], &[malformed]);
        let _: () = unsafe { msg_send![&malformed_content, setUserInfo: &*user_info] };
        let malformed = notification_request("current", &malformed_content);
        assert!(registered_callback(&callbacks, &malformed).is_none());

        let wrong_id = notification_request("old", &valid_content);
        assert!(registered_callback(&callbacks, &wrong_id).is_none());
    }

    #[test]
    fn notification_macos_content_attaches_only_app_icons() {
        let directory = tempfile::tempdir().unwrap();
        let app_icon = Icon::App(include_bytes!("../../icons/notification-message.png").to_vec());
        let lease = crate::notification_icon::materialize_for_test(
            directory.path(),
            content_icon(Some(&app_icon)).unwrap(),
        )
        .unwrap();
        let content = notification_content("title", "body", false, Some(&lease));
        assert_eq!(content.attachments().count(), 1);
        assert!(content_icon(None).is_none());
        assert!(content_icon(Some(&Icon::Message)).is_none());
        assert_eq!(
            notification_content("title", "body", false, None)
                .attachments()
                .count(),
            0
        );
    }

    #[test]
    fn notification_macos_enhanced_categories_match_action_phase_and_copy() {
        assert_eq!(
            category_for(ActionAvailability {
                reveal: true,
                ..Default::default()
            }),
            Some(REVEAL_CATEGORY)
        );
        assert_eq!(
            category_for(ActionAvailability {
                reply: true,
                ..Default::default()
            }),
            Some(REPLY_CATEGORY)
        );
        assert_eq!(
            category_for(ActionAvailability {
                copy: true,
                reply: true,
                ..Default::default()
            }),
            Some(COPY_REPLY_CATEGORY)
        );
        assert_eq!(category_for(ActionAvailability::from(true)), None);
    }

    #[test]
    fn notification_macos_item_clone_retains_the_real_lease_through_late_completion() {
        let directory = tempfile::tempdir().unwrap();
        let app_icon = Icon::App(include_bytes!("../../icons/notification-message.png").to_vec());
        let lease =
            crate::notification_icon::materialize_for_test(directory.path(), &app_icon).unwrap();
        let path = lease.path().to_path_buf();
        let item = Item {
            id: "request".into(),
            token: "token".into(),
            remove_on_completion: Arc::new(AtomicBool::new(false)),
            operations: Operations::default(),
            _icon: Some(lease),
        };
        let late_completion = item.clone();
        drop(item);
        assert!(path.exists());
        drop(late_completion);
        assert!(!path.exists());
    }
    use std::cell::Cell;

    #[test]
    fn notification_mac_post_completion_during_retirement_waits_then_cleans_up() {
        let item = Item {
            id: "request".into(),
            token: "token".into(),
            remove_on_completion: Arc::new(AtomicBool::new(false)),
            operations: Operations::default(),
            _icon: None,
        };
        let os_item = Arc::new(AtomicBool::new(true));
        let callback_owned = Arc::new(AtomicBool::new(true));
        let removals = Arc::new(AtomicUsize::new(0));
        let (start, starting) = std::sync::mpsc::sync_channel(1);
        let (posted, posting) = std::sync::mpsc::sync_channel(1);
        let (done, completed) = std::sync::mpsc::sync_channel(1);
        let completion_item = item.clone();
        let completion_os_item = os_item.clone();
        let completion_callback = callback_owned.clone();
        let completion_removals = removals.clone();
        let completion = std::thread::spawn(move || {
            starting
                .recv_timeout(super::super::owner::PLATFORM_TIMEOUT)
                .unwrap();
            completion_os_item.store(true, Ordering::Release);
            posted.send(()).unwrap();
            completion_item.complete(|_| {
                assert!(!completion_callback.load(Ordering::Acquire));
                completion_os_item.store(false, Ordering::Release);
                completion_removals.fetch_add(1, Ordering::AcqRel);
            });
            done.send(()).unwrap();
        });
        assert!(item.retire(
            |_| {
                os_item.store(false, Ordering::Release);
                removals.fetch_add(1, Ordering::AcqRel);
                // The native post finishes during removal. Its cleanup must wait
                // for retirement to release the operation lock, not reenter it.
                start.send(()).unwrap();
                posting
                    .recv_timeout(super::super::owner::PLATFORM_TIMEOUT)
                    .unwrap();
                true
            },
            |_| {
                callback_owned.store(false, Ordering::Release);
            },
        ));
        completed
            .recv_timeout(super::super::owner::PLATFORM_TIMEOUT)
            .unwrap();
        completion.join().unwrap();
        assert_eq!(removals.load(Ordering::Acquire), 2);
        assert!(!os_item.load(Ordering::Acquire));
        assert!(!callback_owned.load(Ordering::Acquire));
    }

    #[test]
    fn notification_macos_replaced_item_late_completion_cannot_remove_current_native_id() {
        let old = notification_request(
            "logical-id",
            &notification_content("title", "body", false, None),
        );
        let current = notification_request(
            "logical-id",
            &notification_content("title", "body", false, None),
        );
        let mut callbacks = HashMap::from([
            (callback_key(&old).unwrap(), "old"),
            (callback_key(&current).unwrap(), "current"),
        ]);
        let item = Item {
            id: "logical-id".into(),
            token: callback_key(&old).unwrap().1,
            remove_on_completion: Arc::new(AtomicBool::new(false)),
            operations: Operations::default(),
            _icon: None,
        };
        let removals = Cell::new(0);

        item.retire_replaced(&mut callbacks);
        item.complete(|_| removals.set(removals.get() + 1));

        assert!(registered_callback(&callbacks, &old).is_none());
        assert_eq!(registered_callback(&callbacks, &current), Some(&"current"));
        assert_eq!(callbacks.len(), 1);
        assert_eq!(removals.get(), 0);
    }

    #[test]
    fn notification_mac_failed_retirement_keeps_callback_and_fence_for_retry() {
        let item = Item {
            id: "request".into(),
            token: "token".into(),
            remove_on_completion: Arc::new(AtomicBool::new(false)),
            operations: Operations::default(),
            _icon: None,
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
    fn notification_mac_timeout_retires_the_item_before_late_completion() {
        let item = Item {
            id: "request".into(),
            token: "token".into(),
            remove_on_completion: Arc::new(AtomicBool::new(false)),
            operations: Operations::default(),
            _icon: None,
        };
        let removals = Cell::new(0);
        let released = Cell::new(false);

        assert_eq!(
            platform_acceptance_outcome(Err(()), &item, |item| {
                item.retire(
                    |_| {
                        removals.set(removals.get() + 1);
                        true
                    },
                    |_| released.set(true),
                );
            }),
            Err(PostError::Failed)
        );
        assert!(released.get());

        item.complete(|_| removals.set(removals.get() + 1));
        assert_eq!(removals.get(), 2);
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
    fn notification_macos_timeout_quarantines_native_id_after_late_completion() {
        let in_flight = AtomicUsize::new(0);
        let pending = PendingIds::default();
        reserve_pending_id(&in_flight, &pending, "logical-id").unwrap();
        quarantine_pending_id(&pending, "logical-id");
        release_pending_id(&in_flight, &pending, "logical-id");

        assert!(reserve_pending_id(&in_flight, &pending, "logical-id").is_err());
        assert_eq!(in_flight.load(Ordering::Acquire), 0);
    }

    #[test]
    fn notification_macos_pending_id_rejects_overlap_but_allows_completed_replacement() {
        let in_flight = AtomicUsize::new(0);
        let pending = PendingIds::default();
        reserve_pending_id(&in_flight, &pending, "logical-id").unwrap();
        assert!(reserve_pending_id(&in_flight, &pending, "logical-id").is_err());
        assert_eq!(in_flight.load(Ordering::Acquire), 1);

        release_pending_id(&in_flight, &pending, "logical-id");
        reserve_pending_id(&in_flight, &pending, "logical-id").unwrap();
        release_pending_id(&in_flight, &pending, "logical-id");
        assert_eq!(in_flight.load(Ordering::Acquire), 0);
    }

    #[test]
    fn notification_macos_quarantined_ids_exhaust_admission_without_eviction() {
        let in_flight = AtomicUsize::new(0);
        let pending = PendingIds::default();
        for index in 0..super::super::owner::CAPACITY {
            let id = index.to_string();
            reserve_pending_id(&in_flight, &pending, &id).unwrap();
            quarantine_pending_id(&pending, &id);
            release_pending_id(&in_flight, &pending, &id);
        }

        assert!(reserve_pending_id(&in_flight, &pending, "fresh").is_err());
        assert!(reserve_pending_id(&in_flight, &pending, "0").is_err());
        assert_eq!(
            pending.lock().unwrap().quarantined.len(),
            super::super::owner::CAPACITY
        );
        assert_eq!(in_flight.load(Ordering::Acquire), 0);
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
