use eframe::egui;
use objc2::{
    AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained,
    runtime::AnyObject, sel,
};
use objc2_app_kit::{
    NSAccessibility, NSApplication, NSBezierPath, NSBitmapImageRep, NSColor,
    NSCompositingOperation, NSDeviceRGBColorSpace, NSFont, NSFontAttributeName,
    NSFontWeightSemibold, NSForegroundColorAttributeName, NSImage, NSMenu, NSMenuItem, NSStatusBar,
    NSStatusItem, NSStringDrawing,
};
use objc2_foundation::{
    MainThreadMarker, NSDictionary, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    ns_string,
};
use std::cell::Cell;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    ConnectAll,
    DisconnectAll,
}

struct ActionState {
    ctx: egui::Context,
    pending: Cell<Option<Action>>,
}

impl ActionState {
    fn request(&self, action: Action) {
        self.pending.set(Some(action));
        self.ctx.request_repaint();
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ActionState]
    struct MenuActions;

    unsafe impl NSObjectProtocol for MenuActions {}

    impl MenuActions {
        #[unsafe(method(showWindow:))]
        fn show_window(&self, _sender: &NSObject) {
            self.ivars().ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            self.ivars().ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            self.ivars().ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.ivars().ctx.request_repaint();
            #[allow(deprecated)]
            NSApplication::sharedApplication(self.mtm()).activateIgnoringOtherApps(true);
        }

        #[unsafe(method(connectAll:))]
        fn connect_all(&self, _sender: &NSObject) {
            self.ivars().request(Action::ConnectAll);
        }

        #[unsafe(method(disconnectAll:))]
        fn disconnect_all(&self, _sender: &NSObject) {
            self.ivars().request(Action::DisconnectAll);
        }
    }
);

pub struct MenuBar {
    item: Retained<NSStatusItem>,
    icon: Retained<NSImage>,
    connect: Retained<NSMenuItem>,
    disconnect: Retained<NSMenuItem>,
    counts: Option<(usize, usize, usize)>,
    // AppKit holds an unretained action target.
    _actions: Retained<MenuActions>,
}

impl MenuBar {
    pub fn new(ctx: &egui::Context, icon: &egui::IconData) -> Self {
        assert_eq!(
            icon.rgba.len(),
            icon.width as usize * icon.height as usize * 4
        );
        let mtm = MainThreadMarker::new().expect("UI runs on the main thread");
        let actions = MenuActions::alloc(mtm).set_ivars(ActionState {
            ctx: ctx.clone(),
            pending: Cell::new(None),
        });
        let actions: Retained<MenuActions> = unsafe { msg_send![super(actions), init] };
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(26.0);
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let (connect, disconnect, image) = unsafe {
            let show = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("Open ssh-m8"),
                Some(sel!(showWindow:)),
                ns_string!(""),
            );
            show.setTarget(Some(&actions));
            menu.addItem(&show);
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            let connect = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("Connect all tunnels"),
                Some(sel!(connectAll:)),
                ns_string!(""),
            );
            connect.setTarget(Some(&actions));
            menu.addItem(&connect);
            let disconnect = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("Disconnect all tunnels"),
                Some(sel!(disconnectAll:)),
                ns_string!(""),
            );
            disconnect.setTarget(Some(&actions));
            menu.addItem(&disconnect);
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            let quit = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("Quit ssh-m8"),
                Some(sel!(terminate:)),
                ns_string!("q"),
            );
            quit.setTarget(Some(&NSApplication::sharedApplication(mtm)));
            menu.addItem(&quit);

            // Let AppKit own the pixel storage; the Rust icon buffer can then be released.
            let bitmap = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(), std::ptr::null_mut(), icon.width as isize,
                icon.height as isize, 8, 4, true, false, NSDeviceRGBColorSpace,
                (icon.width * 4) as isize, 32,
            ).expect("Allocate menu icon");
            std::ptr::copy_nonoverlapping(icon.rgba.as_ptr(), bitmap.bitmapData(), icon.rgba.len());
            let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(20.0, 20.0));
            image.addRepresentation(&bitmap);
            (connect, disconnect, image)
        };
        item.setMenu(Some(&menu));
        let mut bar = Self {
            item,
            icon: image,
            connect,
            disconnect,
            counts: None,
            _actions: actions,
        };
        bar.update(0, 0, 0);
        bar
    }

    pub fn take_action(&self) -> Option<Action> {
        self._actions.ivars().pending.take()
    }

    pub fn update(&mut self, connected: usize, total: usize, active: usize) {
        if self.counts == Some((connected, total, active)) {
            return;
        }
        self.connect.setEnabled(active < total);
        self.disconnect.setEnabled(active > 0);
        if self.counts.map(|(c, t, _)| (c, t)) != Some((connected, total)) {
            let label = format!("{connected}/{total}");
            let image = badge_image(&self.icon, &label);
            self.item.setLength(image.size().width + 8.0);
            if let Some(button) = self.item.button(self._actions.mtm()) {
                button.setImage(Some(&image));
                let description = NSString::from_str(&format!(
                    "ssh-m8: {connected} of {total} tunnels connected"
                ));
                button.setToolTip(Some(&description));
                button.setAccessibilityLabel(Some(&description));
            }
        }
        self.counts = Some((connected, total, active));
    }
}

#[allow(deprecated)] // lockFocus supports the app's macOS 12 minimum without a drawing-block dependency.
fn badge_image(icon: &NSImage, label: &str) -> Retained<NSImage> {
    unsafe {
        let label = NSString::from_str(label);
        let font = NSFont::monospacedDigitSystemFontOfSize_weight(10.0, NSFontWeightSemibold);
        let ink = NSColor::colorWithSRGBRed_green_blue_alpha(0.08, 0.09, 0.12, 1.0);
        let attrs = NSDictionary::from_slices(
            &[NSFontAttributeName, NSForegroundColorAttributeName],
            &[&*font as &AnyObject, &*ink as &AnyObject],
        );
        let text_size = label.sizeWithAttributes(Some(&attrs));
        let badge_width = (text_size.width + 10.0).ceil();
        let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(18.0 + badge_width, 22.0));
        image.lockFocus();
        icon.drawInRect_fromRect_operation_fraction(
            NSRect::new(NSPoint::new(0.0, 1.0), NSSize::new(20.0, 20.0)),
            NSRect::new(NSPoint::new(0.0, 0.0), icon.size()),
            NSCompositingOperation::SourceOver,
            1.0,
        );
        NSColor::colorWithSRGBRed_green_blue_alpha(0.56, 0.83, 0.73, 1.0).setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            NSRect::new(NSPoint::new(18.0, 3.0), NSSize::new(badge_width, 16.0)),
            8.0,
            8.0,
        )
        .fill();
        label.drawAtPoint_withAttributes(
            NSPoint::new(
                18.0 + (badge_width - text_size.width) / 2.0,
                ((22.0 - text_size.height) / 2.0).floor(),
            ),
            Some(&attrs),
        );
        image.unlockFocus();
        image
    }
}

impl Drop for MenuBar {
    fn drop(&mut self) {
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Renders an AppKit badge; requires a macOS graphics session"]
    fn badge_fits_single_and_multiple_digit_counts() {
        objc2::rc::autoreleasepool(|_| {
            let data =
                objc2_foundation::NSData::with_bytes(include_bytes!("../assets/troll-logo.png"));
            let icon = NSImage::initWithData(NSImage::alloc(), &data).unwrap();
            icon.setSize(NSSize::new(20.0, 20.0));
            let small = badge_image(&icon, "5/5");
            let large = badge_image(&icon, "125/150");
            assert_eq!(small.size().height, 22.0);
            assert!(small.size().width > 35.0);
            assert!(large.size().width > small.size().width);
            if let Some(path) = std::env::var_os("SSH_M8_BADGE_PREVIEW") {
                std::fs::write(path, small.TIFFRepresentation().unwrap().to_vec()).unwrap();
            }
        });
    }

    #[test]
    fn menu_actions_are_consumed_once_and_keep_the_latest_intent() {
        let state = ActionState {
            ctx: egui::Context::default(),
            pending: Cell::new(None),
        };
        state.request(Action::ConnectAll);
        assert_eq!(state.pending.take(), Some(Action::ConnectAll));
        assert_eq!(state.pending.take(), None);
        state.request(Action::ConnectAll);
        state.request(Action::DisconnectAll);
        assert_eq!(state.pending.take(), Some(Action::DisconnectAll));
    }
}
