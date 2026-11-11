use super::{
    owner::Signal,
    windows_codec::{self, ActivationAction},
    ActionAvailability, PostError,
};
use crate::notification_icon::{self, Icon, Lease};
use tauri::{AppHandle, Manager};
use windows::{
    core::{IInspectable, Interface, HSTRING},
    Data::Xml::Dom::XmlDocument,
    Foundation::TypedEventHandler,
    Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
    UI::Notifications::{
        NotificationSetting, ToastActivatedEventArgs, ToastDismissalReason,
        ToastDismissedEventArgs, ToastFailedEventArgs, ToastNotification, ToastNotificationManager,
        ToastNotifier,
    },
};

pub fn prepare() -> Option<()> {
    // Keep the legacy development identity fallback for uninstalled binaries.
    let executable = std::env::current_exe().ok()?;
    let parent = executable.parent()?;
    if parent.ends_with("target/debug") || parent.ends_with("target/release") {
        return None;
    }
    Some(())
}

pub struct Backend {
    notifier: ToastNotifier,
    app_id: HSTRING,
    app: AppHandle,
}
pub struct Item {
    toast: ToastNotification,
    tag: HSTRING,
    activated: Option<i64>,
    dismissed: Option<i64>,
    failed: Option<i64>,
    _icon: Option<Lease>,
    _action_icons: Option<ActionIconLeases>,
}

impl Drop for Item {
    fn drop(&mut self) {
        if let Some(token) = self.activated {
            let _ = self.toast.RemoveActivated(token);
        }
        if let Some(token) = self.dismissed {
            let _ = self.toast.RemoveDismissed(token);
        }
        if let Some(token) = self.failed {
            let _ = self.toast.RemoveFailed(token);
        }
    }
}

impl Backend {
    pub fn new(app: &tauri::AppHandle, _: ()) -> Option<Self> {
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED).ok()?;
        }
        let app_id = HSTRING::from(&app.config().identifier);
        match ToastNotificationManager::CreateToastNotifierWithId(&app_id) {
            Ok(notifier) => Some(Self {
                notifier,
                app_id,
                app: app.clone(),
            }),
            Err(_) => {
                unsafe {
                    RoUninitialize();
                }
                None
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn post(
        &mut self,
        id: &str,
        presentation_generation: u64,
        title: &str,
        body: &str,
        sound: bool,
        icon: Option<&Icon>,
        actions: ActionAvailability,
        signal: Signal,
    ) -> Result<Item, PostError> {
        self.post_inner(
            id,
            presentation_generation,
            title,
            body,
            sound,
            icon,
            actions,
            signal,
        )
        .map_err(|_| PostError::Failed)
    }

    pub fn ensure_authorized(&mut self) -> Result<(), PostError> {
        match self.notifier.Setting() {
            Ok(NotificationSetting::Enabled) => Ok(()),
            Ok(_) => Err(PostError::NotAllowed),
            Err(_) => Err(PostError::Failed),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn post_inner(
        &mut self,
        id: &str,
        _: u64,
        title: &str,
        body: &str,
        sound: bool,
        icon: Option<&Icon>,
        actions: ActionAvailability,
        signal: Signal,
    ) -> windows::core::Result<Item> {
        let icon = icon.and_then(|icon| notification_icon::materialize(&self.app, icon));
        let icon_uri = icon
            .as_ref()
            .and_then(|lease| url::Url::from_file_path(lease.path()).ok())
            .map(|url| url.to_string());
        let action_icons = action_icons(&self.app, actions);
        let document = XmlDocument::new()?;
        document.LoadXml(&HSTRING::from(super::windows_xml::toast_xml_with_icons(
            title,
            body,
            icon_uri.as_deref(),
            super::windows_audio(sound),
            self.app
                .state::<crate::AppState>()
                .notifications
                .persistence(),
            actions,
            action_icons.as_ref().map(Into::into),
        )))?;
        let toast = ToastNotification::CreateToastNotification(&document)?;
        let tag = HSTRING::from(id);
        toast.SetTag(&tag)?;
        toast.SetGroup(&HSTRING::from("peppy-running"))?;
        let mut item = Item {
            toast,
            tag,
            activated: None,
            dismissed: None,
            failed: None,
            _icon: icon,
            _action_icons: action_icons,
        };
        let activate = signal.clone();
        item.activated = Some(item.toast.Activated(&TypedEventHandler::<
            ToastNotification,
            IInspectable,
        >::new(move |_, args| {
            let args = args
                .as_ref()
                .and_then(|args| args.cast::<ToastActivatedEventArgs>().ok());
            let argument = args.as_ref().and_then(windows_codec::action_argument);
            match windows_codec::parse_action(argument.as_deref()) {
                Some(ActivationAction::Copy) => activate.copy(),
                Some(ActivationAction::Reveal) => activate.reveal(),
                Some(ActivationAction::Reply) => {
                    // Missing, malformed, and non-string input values fail closed.
                    if let Some(body) = args.as_ref().and_then(windows_codec::reply_body) {
                        activate.reply(body);
                    }
                }
                Some(ActivationAction::Click) => activate.click(),
                None => {}
            }
            Ok(())
        }))?);
        let dismiss = signal.clone();
        item.dismissed = Some(item.toast.Dismissed(&TypedEventHandler::<
            ToastNotification,
            ToastDismissedEventArgs,
        >::new(move |_, args| {
            if let Some(args) = args.as_ref() {
                match args.Reason()? {
                    ToastDismissalReason::TimedOut => dismiss.dismissed(true),
                    ToastDismissalReason::UserCanceled
                    | ToastDismissalReason::ApplicationHidden => dismiss.dismissed(false),
                    _ => {}
                }
            }
            Ok(())
        }))?);
        item.failed = Some(item.toast.Failed(&TypedEventHandler::<
            ToastNotification,
            ToastFailedEventArgs,
        >::new(move |_, _| {
            eprintln!("peppy: asynchronous native notification post failed");
            signal.retire();
            Ok(())
        }))?);
        // Item's Drop rolls back all already-registered tokens on every error path.
        self.notifier.Show(&item.toast)?;
        Ok(item)
    }

    pub fn remove(&mut self, item: &Item) -> bool {
        let hidden = self.notifier.Hide(&item.toast).is_ok();
        let removed = ToastNotificationManager::History()
            .and_then(|history| {
                history.RemoveGroupedTagWithId(
                    &item.tag,
                    &HSTRING::from("peppy-running"),
                    &self.app_id,
                )
            })
            .is_ok();
        hidden && removed
    }

    /// A replacement has already been posted under the same tag. Dropping the
    /// old event registrations is sufficient; removing history here could erase
    /// the newly posted toast.
    pub fn retire_replaced(&mut self, item: &Item) -> bool {
        let _ = item
            .toast
            .RemoveActivated(item.activated.unwrap_or_default());
        let _ = item
            .toast
            .RemoveDismissed(item.dismissed.unwrap_or_default());
        let _ = item.toast.RemoveFailed(item.failed.unwrap_or_default());
        true
    }
}

const SHOW_ICON: &[u8] = include_bytes!("../../icons/notification-actions/show.png");
const SEND_ICON: &[u8] = include_bytes!("../../icons/notification-actions/send.png");
const COPY_ICON: &[u8] = include_bytes!("../../icons/notification-actions/copy.png");
const DISMISS_ICON: &[u8] = include_bytes!("../../icons/notification-actions/dismiss.png");

struct ActionIconLeases {
    show: Lease,
    send: Lease,
    copy: Lease,
    dismiss: Lease,
}

impl From<&ActionIconLeases> for super::windows_xml::ActionIconUris<'_> {
    fn from(leases: &ActionIconLeases) -> Self {
        let uri = |lease: &Lease| {
            url::Url::from_file_path(lease.path())
                .ok()
                .map(|url| std::borrow::Cow::Owned(url.to_string()))
        };
        Self {
            show: uri(&leases.show).unwrap_or_default(),
            send: uri(&leases.send).unwrap_or_default(),
            copy: uri(&leases.copy).unwrap_or_default(),
            dismiss: uri(&leases.dismiss).unwrap_or_default(),
        }
    }
}

fn action_icons(app: &AppHandle, actions: ActionAvailability) -> Option<ActionIconLeases> {
    if !actions.copy && !actions.reveal && !actions.reply {
        return None;
    }
    Some(ActionIconLeases {
        show: notification_icon::materialize_bytes(app, SHOW_ICON)?,
        send: notification_icon::materialize_bytes(app, SEND_ICON)?,
        copy: notification_icon::materialize_bytes(app, COPY_ICON)?,
        dismiss: notification_icon::materialize_bytes(app, DISMISS_ICON)?,
    })
}

impl Drop for Backend {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}
