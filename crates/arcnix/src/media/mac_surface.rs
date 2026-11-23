//! Keep an embedded WKWebView visible and interactive only where the desktop
//! exposes it. Shared by the Code and pgAdmin windows.
use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::NSView;
use objc2_core_graphics::{CGMutablePath, CGPath};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use objc2_quartz_core::{CAShapeLayer, CATransaction};

use crate::native_viewport::Viewport;

define_class!(
    #[unsafe(super(NSView))]
    #[name = "ArcnixClipView"]
    #[ivars = Retained<CAShapeLayer>]
    struct ClipView;

    impl ClipView {
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
            self.hit_visible_child(point)
        }
    }
);

impl ClipView {
    fn hit_visible_child(&self, point: NSPoint) -> Option<Retained<NSView>> {
        let local = self.convertPoint_fromView(point, unsafe { self.superview() }.as_deref());
        let mask_point = self.convertPointToLayer(local);
        let path = self.ivars().path()?;
        // A null transform uses the mask's own coordinate system.
        if !unsafe { CGPath::contains_point(Some(&path), std::ptr::null(), mask_point, false) } {
            return None;
        }
        let hit: Option<Retained<NSView>> = unsafe { msg_send![super(self), hitTest: point] };
        // When the workbench is hidden, pass the event to the desktop below.
        hit.filter(|view| !std::ptr::eq::<NSView>(&**view, &**self))
    }
}

pub(crate) struct Surface(Retained<ClipView>);

impl Surface {
    pub(crate) fn attach(child: &NSView) -> Result<Self> {
        let parent = unsafe { child.superview() }.context("Missing WebView parent")?;
        let allocated = ClipView::alloc(child.mtm()).set_ivars(CAShapeLayer::layer());
        let wrapper: Retained<ClipView> =
            unsafe { msg_send![super(allocated), initWithFrame: child.frame()] };
        let surface = Self(wrapper);
        let wrapper = &surface.0;
        wrapper.setAutoresizingMask(child.autoresizingMask());
        parent.addSubview(wrapper);
        wrapper.setWantsLayer(true);
        let layer = wrapper.layer().context("Missing clipping layer")?;
        // This mask belongs exclusively to the wrapper's backing layer.
        unsafe { layer.setMask(Some(wrapper.ivars())) };
        wrapper.addSubview(child);
        Ok(surface)
    }

    pub(crate) fn position(&self, child: &NSView, viewport: &Viewport) -> Result<()> {
        let wrapper = &self.0;
        let area = viewport.area.context("Missing WebView bounds")?;
        let visible = viewport.visible_rects();
        if visible.is_empty() {
            child.setHidden(true);
            return Ok(());
        }
        let parent = unsafe { wrapper.superview() }.context("Missing clipping surface parent")?;
        let scale = parent
            .window()
            .context("Missing WebView window")?
            .backingScaleFactor();
        let points = |px: i32| f64::from(px) / scale;
        let parent_bounds = parent.bounds();
        let y = if parent.isFlipped() {
            points(area.top)
        } else {
            parent_bounds.size.height - points(area.bottom)
        };
        let frame = NSRect::new(
            NSPoint::new(
                parent_bounds.origin.x + points(area.left),
                parent_bounds.origin.y + y,
            ),
            NSSize::new(points(area.width()), points(area.height())),
        );
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        wrapper.setFrame(frame);
        child.setFrame(wrapper.bounds());
        let path = CGMutablePath::new();
        for rect in visible {
            // ClipView uses AppKit's default bottom-left origin.
            let local = NSRect::new(
                NSPoint::new(
                    points(rect.left - area.left),
                    points(area.bottom - rect.bottom),
                ),
                NSSize::new(points(rect.width()), points(rect.height())),
            );
            unsafe {
                CGMutablePath::add_rect(
                    Some(&path),
                    std::ptr::null(),
                    wrapper.convertRectToLayer(local),
                )
            };
        }
        wrapper
            .ivars()
            .setFrame(wrapper.convertRectToLayer(wrapper.bounds()));
        wrapper.ivars().setPath(Some(&path));
        child.setHidden(false);
        CATransaction::commit();
        Ok(())
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.0.removeFromSuperview();
    }
}
