use std::cell::{Cell, RefCell};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{define_class, msg_send, AnyThread, DeclaredClass, MainThreadMarker};
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSAutoresizingMaskOptions, NSColor, NSFont,
    NSFontAttributeName, NSForegroundColorAttributeName, NSImage, NSImageView, NSStatusBarButton,
    NSStatusItem, NSView,
};
use objc2_foundation::{
    NSAttributedString, NSAttributedStringKey, NSDictionary, NSPoint, NSRect, NSSize, NSString,
};

const ICON_WIDTH: f64 = 18.0;
const TEXT_MARGIN: f64 = 2.0;
const ONE_LINE_FONT_SIZE: f64 = 12.0;
const TWO_LINE_FONT_SIZE: f64 = 9.0;
const TWO_LINE_OVERLAP: f64 = 2.0;

#[derive(Debug)]
pub(crate) struct TrayTitleViewIvars {
    first_view: Retained<NSImageView>,
    second_view: Retained<NSImageView>,
    status_item: Retained<NSStatusItem>,
    button: Retained<NSStatusBarButton>,
    icon_view: Retained<NSImageView>,
    button_height: f64,
    last_width: Cell<f64>,
    last_first: RefCell<Option<String>>,
    last_second: RefCell<Option<String>>,
    last_font_size: Cell<f64>,
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
    let view = TrayTitleView::new(mtm, status_item.clone(), button.clone(), button_height);
    button.addSubview(&view);
    button.setImage(None);
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
        button: Retained<NSStatusBarButton>,
        button_height: f64,
    ) -> Retained<Self> {
        let first_view = make_image_view(mtm);
        let second_view = make_image_view(mtm);
        let icon_view = match button.image() {
            Some(image) => NSImageView::imageViewWithImage(&image, mtm),
            None => NSImageView::initWithFrame(
                mtm.alloc(),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(ICON_WIDTH, button_height)),
            ),
        };
        let icon_width = button
            .image()
            .map(|image| image.size().width)
            .unwrap_or(ICON_WIDTH)
            .max(1.0);
        icon_view.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(icon_width, button_height),
        ));
        let this = mtm.alloc().set_ivars(TrayTitleViewIvars {
            first_view,
            second_view,
            status_item,
            button,
            icon_view,
            button_height,
            last_width: Cell::new(0.0),
            last_first: RefCell::new(None),
            last_second: RefCell::new(None),
            last_font_size: Cell::new(0.0),
        });
        let view: Retained<Self> = unsafe {
            msg_send![
                super(this),
                initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, button_height))
            ]
        };
        view.addSubview(&view.ivars().icon_view);
        view.addSubview(&view.ivars().first_view);
        view.addSubview(&view.ivars().second_view);
        view
    }

    pub(crate) fn refresh_icon(&self) {
        if let Some(image) = self.ivars().button.image() {
            self.apply_icon_image(&image);
            self.ivars().button.setImage(None);
        }
    }

    fn apply_icon_image(&self, image: &NSImage) {
        let icon_width = image.size().width.max(1.0);
        self.ivars().icon_view.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(icon_width, self.ivars().button_height),
        ));
        self.ivars().icon_view.setImage(Some(image));
    }

    pub(crate) fn update(&self, first: Option<&str>, second: Option<&str>) {
        let line_count = usize::from(first.is_some()) + usize::from(second.is_some());
        let font_size = if line_count >= 2 {
            TWO_LINE_FONT_SIZE
        } else {
            ONE_LINE_FONT_SIZE
        };
        let font = NSFont::systemFontOfSize(font_size);
        let refresh = self.ivars().last_first.borrow().as_deref() != first
            || self.ivars().last_second.borrow().as_deref() != second
            || self.ivars().last_font_size.get() != font_size;

        let first_size = apply_text_image(&self.ivars().first_view, first, &font, refresh);
        let second_size = apply_text_image(&self.ivars().second_view, second, &font, refresh);
        if refresh {
            *self.ivars().last_first.borrow_mut() = first.map(str::to_string);
            *self.ivars().last_second.borrow_mut() = second.map(str::to_string);
            self.ivars().last_font_size.set(font_size);
        }

        let max_text_width = first_size.width.max(second_size.width);
        let mut measured_height: f64 = font_size * 1.1;
        if first.is_some() {
            measured_height = measured_height.max(first_size.height);
        }
        if second.is_some() {
            measured_height = measured_height.max(second_size.height);
        }

        let line_height = measured_height;
        let height = self.ivars().button_height;
        let icon_width = self.ivars().icon_view.frame().size.width.max(1.0);
        let icon_x = if line_count == 0 {
            let button_width = self.ivars().button.bounds().size.width;
            ((button_width - icon_width) / 2.0).max(0.0)
        } else {
            0.0
        };
        self.ivars().icon_view.setFrame(NSRect::new(
            NSPoint::new(icon_x, 0.0),
            NSSize::new(icon_width, height),
        ));
        let text_x = icon_x + icon_width + TEXT_MARGIN;
        let text_right = text_x + max_text_width + TEXT_MARGIN;
        if line_count >= 2 {
            let middle = height / 2.0;
            let overlap = TWO_LINE_OVERLAP / 2.0;
            set_view_frame(
                &self.ivars().first_view,
                first_size,
                text_right,
                middle - overlap,
            );
            set_view_frame(
                &self.ivars().second_view,
                second_size,
                text_right,
                middle - line_height + overlap,
            );
        } else {
            let y = ((height - line_height) / 2.0).max(0.0);
            if first.is_some() {
                set_view_frame(&self.ivars().first_view, first_size, text_right, y);
            }
            if second.is_some() {
                set_view_frame(&self.ivars().second_view, second_size, text_right, y);
            }
        }

        let width = if line_count == 0 {
            icon_width + 6.0
        } else {
            (icon_width + max_text_width + TEXT_MARGIN * 2.0).max(icon_width + 6.0)
        };
        if width != self.ivars().last_width.get() {
            self.ivars().status_item.setLength(width);
            self.ivars().last_width.set(width);
        }
        self.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(width, height),
        ));
    }
}

fn make_image_view(mtm: MainThreadMarker) -> Retained<NSImageView> {
    let view = NSImageView::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
    );
    view.setEditable(false);
    view
}

fn apply_text_image(
    view: &NSImageView,
    text: Option<&str>,
    font: &NSFont,
    refresh: bool,
) -> NSSize {
    match text {
        None => {
            if refresh {
                view.setImage(None);
            }
            view.setHidden(true);
            NSSize::new(0.0, 0.0)
        }
        Some(text) => {
            if refresh {
                view.setImage(Some(&render_text_template(text, font)));
            }
            view.setHidden(false);
            view.image().map_or(NSSize::new(0.0, 0.0), |img| img.size())
        }
    }
}

fn set_view_frame(view: &NSImageView, size: NSSize, text_right: f64, y: f64) {
    view.setFrame(NSRect::new(
        NSPoint::new((text_right - size.width).max(0.0), y),
        size,
    ));
}

fn render_text_template(text: &str, font: &NSFont) -> Retained<NSImage> {
    let color = NSColor::blackColor();
    let font_obj: &AnyObject = font;
    let color_obj: &AnyObject = &color;
    // SAFETY: 属性名是 AppKit 的常量 static; attrs 只包含 NSFont 和 NSColor,
    // 分别对应 font 与 foreground. 黑色文字作为 template 遮罩.
    let attrs: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> = unsafe {
        NSDictionary::from_slices(
            &[NSFontAttributeName, NSForegroundColorAttributeName],
            &[font_obj, color_obj],
        )
    };
    let attributed = unsafe {
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &NSString::from_str(text),
            Some(&attrs),
        )
    };
    let measured = attributed.size();
    let size = NSSize::new(measured.width.ceil().max(1.0), measured.height.ceil().max(1.0));
    let block = RcBlock::new(move |_rect: NSRect| {
        attributed.drawAtPoint(NSPoint::new(0.0, 0.0));
        Bool::YES
    });
    let image = NSImage::imageWithSize_flipped_drawingHandler(size, false, &block);
    image.setTemplate(true);
    image
}
