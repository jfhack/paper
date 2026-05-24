use std::path::PathBuf;
use std::sync::Mutex;

static PENDING_OPENS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

pub fn take_pending() -> Vec<PathBuf> {
    match PENDING_OPENS.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => Vec::new(),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn install_handler() {}

#[cfg(target_os = "macos")]
pub fn install_handler() {
    macos::install();
}

#[cfg(target_os = "macos")]
mod macos {
    use super::PENDING_OPENS;
    use std::path::PathBuf;

    use objc2::rc::Retained;
    use objc2::runtime::NSObject;
    use objc2::{declare_class, msg_send, msg_send_id, mutability, sel, ClassType, DeclaredClass};
    use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager};

    const K_CORE_EVENT_CLASS: u32 = 0x6165_7674;
    const K_AE_OPEN_DOCUMENTS: u32 = 0x6f64_6f63;
    const KEY_DIRECT_OBJECT: u32 = 0x2d2d_2d2d;

    declare_class!(
        struct OpenHandler;

        unsafe impl ClassType for OpenHandler {
            type Super = NSObject;
            type Mutability = mutability::InteriorMutable;
            const NAME: &'static str = "PaperOpenHandler";
        }

        impl DeclaredClass for OpenHandler {}

        unsafe impl OpenHandler {
            #[method(handleAppleEvent:withReplyEvent:)]
            unsafe fn handle_apple_event(
                &self,
                event: &NSAppleEventDescriptor,
                _reply: &NSAppleEventDescriptor,
            ) {
                let direct: Option<Retained<NSAppleEventDescriptor>> =
                    msg_send_id![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT];
                let Some(direct) = direct else {
                    return;
                };
                let count = direct.numberOfItems();
                let mut found: Vec<PathBuf> = Vec::new();
                let mut i: isize = 1;
                while i <= count {
                    if let Some(item) = direct.descriptorAtIndex(i) {
                        if let Some(url) = item.fileURLValue() {
                            if let Some(path) = url.path() {
                                found.push(PathBuf::from(path.to_string()));
                            }
                        }
                    }
                    i += 1;
                }
                if !found.is_empty() {
                    if let Ok(mut q) = PENDING_OPENS.lock() {
                        q.extend(found);
                    }
                }
            }
        }
    );

    pub fn install() {
        unsafe {
            let handler: Retained<OpenHandler> = msg_send_id![OpenHandler::alloc(), init];
            let manager = NSAppleEventManager::sharedAppleEventManager();
            let _: () = msg_send![
                &manager,
                setEventHandler: &*handler,
                andSelector: sel!(handleAppleEvent:withReplyEvent:),
                forEventClass: K_CORE_EVENT_CLASS,
                andEventID: K_AE_OPEN_DOCUMENTS,
            ];
            std::mem::forget(handler);
        }
    }
}
