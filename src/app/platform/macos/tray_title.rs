use std::cell::Cell;

use objc2::rc::Retained;
use objc2::{define_class, msg_send, DeclaredClass, MainThreadMarker};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSColor, NSFont, NSLineBreakMode, NSStatusItem, NSTextAlignment,
    NSTextField, NSView,
};
use objc2_foundation::{NSString, NSPoint, NSRect, NSSize};

const ICON_WIDTH: f64 = 22.0;
const TEXT_MARGIN: f64 = 4.0;
const ONE_LINE_FONT_SIZE: f64 = 12.0;
const TWO_LINE_FONT_SIZE: f64 = 9.0;

#[derive(Debug)]
struct TrayTitleViewIvars {
    first_label: Retained<NSTextField>,
    second_label: Retained<NSTextField>,
    status_item: Retained<NSStatusItem>,
    button_height: f64,
    last_width: Cell<f64>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[name = "CodexSwitchTrayTitleView"]
    #[ivars = TrayTitleViewIvars]
    pub(crate) struct TrayTitleView;

    impl TrayTitleView {
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
            None
        }
    }
);

pub(crate) fn install(
    status_item: &Retained<NSStatusItem>,
    mtm: MainThreadMarker,
) -> Retained<TrayTitleView> {
    let button = status_item
        .button(mtm)
        .expect("tray status item must have a button");
    let button_height = button.bounds().size.height;
    let view = TrayTitleView::new(mtm, status_item.clone(), button_height);
    button.addSubview(&view);
    view.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(ICON_WIDTH, button_height),
    ));
    view.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    view
}

impl TrayTitleView {
    fn new(
        mtm: MainThreadMarker,
        status_item: Retained<NSStatusItem>,
        button_height: f64,
    ) -> Retained<Self> {
        let first_label = make_label(mtm, "");
        let second_label = make_label(mtm, "");
        let this = mtm.alloc().set_ivars(TrayTitleViewIvars {
            first_label,
            second_label,
            status_item,
            button_height,
            last_width: Cell::new(0.0),
        });
        let view: Retained<Self> = unsafe {
            msg_send![
                super(this),
                initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, button_height))
            ]
        };
        view.addSubview(&view.ivars().first_label);
        view.addSubview(&view.ivars().second_label);
        view
    }

    pub(crate) fn update(&self, first: Option<&str>, second: Option<&str>) {
        let line_count = usize::from(first.is_some()) + usize::from(second.is_some());
        let font_size = if line_count >= 2 {
            TWO_LINE_FONT_SIZE
        } else {
            ONE_LINE_FONT_SIZE
        };
        let font = NSFont::systemFontOfSize(font_size);
        let color = NSColor::labelColor();

        self.apply_label(&self.ivars().first_label, first, &font, &color);
        self.apply_label(&self.ivars().second_label, second, &font, &color);

        let mut max_text_width: f64 = 0.0;
        let mut measured_height: f64 = font_size * 1.1;
        if first.is_some() {
            let size = self.ivars().first_label.frame().size;
            max_text_width = max_text_width.max(size.width);
            measured_height = measured_height.max(size.height);
        }
        if second.is_some() {
            let size = self.ivars().second_label.frame().size;
            max_text_width = max_text_width.max(size.width);
            measured_height = measured_height.max(size.height);
        }

        let line_height = measured_height;
        let text_x = ICON_WIDTH + TEXT_MARGIN;
        let height = self.ivars().button_height;
        let text_width = max_text_width + TEXT_MARGIN;
        if line_count >= 2 {
            let middle = height / 2.0;
            self.ivars().first_label.setFrame(NSRect::new(
                NSPoint::new(text_x, middle + 1.0),
                NSSize::new(text_width, line_height),
            ));
            self.ivars().second_label.setFrame(NSRect::new(
                NSPoint::new(text_x, middle - line_height - 1.0),
                NSSize::new(text_width, line_height),
            ));
        } else {
            let y = ((height - line_height) / 2.0).max(0.0);
            self.ivars().first_label.setFrame(NSRect::new(
                NSPoint::new(text_x, y),
                NSSize::new(text_width, line_height),
            ));
        }

        let width = (ICON_WIDTH + max_text_width + TEXT_MARGIN * 2.0).max(ICON_WIDTH + 6.0);
        if width != self.ivars().last_width.get() {
            self.ivars().status_item.setLength(width);
            self.ivars().last_width.set(width);
        }
        self.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(width, height),
        ));
    }

    fn apply_label(
        &self,
        label: &NSTextField,
        text: Option<&str>,
        font: &NSFont,
        color: &NSColor,
    ) {
        label.setFont(Some(font));
        label.setTextColor(Some(color));
        label.setStringValue(&NSString::from_str(text.unwrap_or("")));
        label.setHidden(text.is_none());
        if text.is_some() {
            label.sizeToFit();
        }
    }
}

fn make_label(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setBezeled(false);
    label.setBordered(false);
    label.setDrawsBackground(false);
    label.setEditable(false);
    label.setSelectable(false);
    label.setAlignment(NSTextAlignment::Left);
    label.setLineBreakMode(NSLineBreakMode::ByClipping);
    label.setUsesSingleLineMode(false);
    label
}
