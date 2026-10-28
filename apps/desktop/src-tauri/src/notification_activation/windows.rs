use super::owner::Signal;
use windows::{
    core::{IInspectable, HSTRING},
    Data::Xml::Dom::XmlDocument,
    Foundation::TypedEventHandler,
    Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
    UI::Notifications::{
        ToastDismissalReason, ToastDismissedEventArgs, ToastFailedEventArgs, ToastNotification,
        ToastNotificationManager, ToastNotifier,
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
}
pub struct Item {
    toast: ToastNotification,
    tag: HSTRING,
    activated: Option<i64>,
    dismissed: Option<i64>,
    failed: Option<i64>,
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
            Ok(notifier) => Some(Self { notifier, app_id }),
            Err(_) => {
                unsafe {
                    RoUninitialize();
                }
                None
            }
        }
    }

    pub fn post(&mut self, id: &str, title: &str, body: &str, signal: Signal) -> Result<Item, ()> {
        self.post_inner(id, title, body, signal).map_err(|_| ())
    }

    fn post_inner(
        &mut self,
        id: &str,
        title: &str,
        body: &str,
        signal: Signal,
    ) -> windows::core::Result<Item> {
        let document = XmlDocument::new()?;
        document.LoadXml(&HSTRING::from(format!(
            "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual><audio silent=\"true\"/></toast>",
            escape(title), escape(body))))?;
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
        };
        let click = signal.clone();
        item.activated = Some(item.toast.Activated(&TypedEventHandler::<
            ToastNotification,
            IInspectable,
        >::new(move |_, _| {
            click.click();
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
}

impl Drop for Backend {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
