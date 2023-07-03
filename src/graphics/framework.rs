// taken from https://github.com/gfx-rs/wgpu/blob/trunk/examples/common/src/framework.rs
use raw_window_handle::{
    HasRawDisplayHandle, HasRawWindowHandle, RawDisplayHandle, RawWindowHandle,
    WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Modifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
};
use std::borrow::Cow;
use std::future::Future;
use std::time::Instant;
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
    Connection, Proxy, QueueHandle,
};
use xkbcommon::xkb::keysyms;

#[allow(dead_code)]
pub enum ShaderStage {
    Vertex,
    Fragment,
    Compute,
}

pub struct Wallpaper {
    pub registry_state: RegistryState,
    pub seat_state: SeatState,
    pub output_state: OutputState,
    pub exit: bool,
    pub first_configure: bool,
    pub width: u32,
    pub height: u32,
    pub adapter: wgpu::Adapter,
    pub queue: wgpu::Queue,
    pub device: wgpu::Device,
    pub surface: wgpu::Surface,
    pub wl_surface: wl_surface::WlSurface,

    pub shift: Option<u32>,
    pub layer: LayerSurface,
    pub keyboard: Option<wl_keyboard::WlKeyboard>,
    pub keyboard_focus: bool,
    pub pointer: Option<wl_pointer::WlPointer>,
}
pub trait WgpuConfig: 'static + Sized {
    fn optional_features() -> wgpu::Features {
        wgpu::Features::empty()
    }
    fn required_features() -> wgpu::Features {
        wgpu::Features::empty()
    }
    fn required_downlevel_capabilities() -> wgpu::DownlevelCapabilities {
        wgpu::DownlevelCapabilities {
            flags: wgpu::DownlevelFlags::empty(),
            shader_model: wgpu::ShaderModel::Sm5,
            ..wgpu::DownlevelCapabilities::default()
        }
    }
    fn required_limits() -> wgpu::Limits {
        wgpu::Limits::downlevel_webgl2_defaults() // These downlevel limits will allow the code to run on all possible hardware
    }
}

pub async fn setup<E: WgpuConfig>() {
    env_logger::init();
    // All Wayland apps start by connecting the compositor (server).
    let conn = Connection::connect_to_env().unwrap();

    // Enumerate the list of globals to get the protocols the server implements.
    let (globals, mut event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();

    // The compositor (not to be confused with the server which is commonly called the compositor) allows
    // configuring surfaces to be presented.
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor is not available");
    let surface = compositor.create_surface(&qh);
    // This app uses the wlr layer shell, which may not be available with every compositor.
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell is not available");
    // Initialize wgpu
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    log::info!("Initializing layer_shell");
    // And then we create the layer shell.
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Background,
        Some("simple_layer"),
        None,
    );
    // Create the raw window handle for the surface.
    let handle = {
        let mut handle = WaylandDisplayHandle::empty();
        handle.display = conn.backend().display_ptr() as *mut _;
        let display_handle = RawDisplayHandle::Wayland(handle);

        let mut handle = WaylandWindowHandle::empty();
        let wl_surface = layer.wl_surface();
        handle.surface = wl_surface.id().as_ptr() as *mut _;
        let window_handle = RawWindowHandle::Wayland(handle);

        /// https://github.com/rust-windowing/raw-window-handle/issues/49
        struct YesRawWindowHandleImplementingHasRawWindowHandleIsUnsound(
            RawDisplayHandle,
            RawWindowHandle,
        );

        unsafe impl HasRawDisplayHandle for YesRawWindowHandleImplementingHasRawWindowHandleIsUnsound {
            fn raw_display_handle(&self) -> RawDisplayHandle {
                self.0
            }
        }

        unsafe impl HasRawWindowHandle for YesRawWindowHandleImplementingHasRawWindowHandleIsUnsound {
            fn raw_window_handle(&self) -> RawWindowHandle {
                self.1
            }
        }

        YesRawWindowHandleImplementingHasRawWindowHandleIsUnsound(display_handle, window_handle)
    };

    // A layer surface is created from a surface.
    let surface = unsafe { instance.create_surface(&handle).unwrap() };

    // Pick a supported adapter
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .expect("Failed to find suitable adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default(), None))
        .expect("Failed to request device");
    // Configure the layer surface, providing things like the anchor on screen, desired size and the keyboard
    // interactivity
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::RIGHT | Anchor::LEFT);
    layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
    layer.set_exclusive_zone(-1);
    layer.commit();
    let wl_surface = layer.wl_surface().clone();

    let adapter_info = adapter.get_info();
    println!("Using {} ({:?})", adapter_info.name, adapter_info.backend);

    let optional_features = E::optional_features();
    let required_features = E::required_features();
    let adapter_features = adapter.features();
    assert!(
        adapter_features.contains(required_features),
        "Adapter does not support required features for this example: {:?}",
        required_features - adapter_features
    );

    let required_downlevel_capabilities = E::required_downlevel_capabilities();
    let downlevel_capabilities = adapter.get_downlevel_capabilities();
    assert!(
        downlevel_capabilities.shader_model >= required_downlevel_capabilities.shader_model,
        "Adapter does not support the minimum shader model required to run this example: {:?}",
        required_downlevel_capabilities.shader_model
    );
    assert!(
        downlevel_capabilities
            .flags
            .contains(required_downlevel_capabilities.flags),
        "Adapter does not support the downlevel capabilities required to run this example: {:?}",
        required_downlevel_capabilities.flags - downlevel_capabilities.flags
    );

    // Make sure we use the texture resolution limits from the adapter, so we can support images the size of the surface.
    let needed_limits = E::required_limits().using_resolution(adapter.limits());

    let trace_dir = std::env::var("WGPU_TRACE");
    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: None,
                features: (optional_features & adapter_features) | required_features,
                limits: needed_limits,
            },
            trace_dir.ok().as_ref().map(std::path::Path::new),
        )
        .await
        .expect("Unable to find a suitable GPU adapter!");

    let mut w = Wallpaper {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),

        exit: false,
        first_configure: true,
        width: 256,
        height: 256,
        device,
        wl_surface,
        surface,
        adapter,
        queue,
        shift: None,
        layer,
        keyboard: None,
        keyboard_focus: false,
        pointer: None,
    };
println!("Starting event loop");
    loop {
        event_queue.blocking_dispatch(&mut w).unwrap();

        if w.exit {
            log::info!("Exiting");
            break;
        }
    }
}
// impl CompositorHandler for Wallpaper {
//     fn scale_factor_changed(
//         &mut self,
//         _conn: &Connection,
//         _qh: &QueueHandle<Self>,
//         _surface: &wl_surface::WlSurface,
//         _new_factor: i32,
//     ) {
//         // Not needed for this example.
//     }

//     fn frame(
//         &mut self,
//         _conn: &Connection,
//         qh: &QueueHandle<Self>,
//         _surface: &wl_surface::WlSurface,
//         _time: u32,
//     ) {
//         println!("frame");
//         self.draw(qh);
//     }
// }

// impl OutputHandler for Wallpaper {
//     fn output_state(&mut self) -> &mut OutputState {
//         &mut self.output_state
//     }

//     fn new_output(
//         &mut self,
//         _conn: &Connection,
//         _qh: &QueueHandle<Self>,
//         _output: wl_output::WlOutput,
//     ) {
//     }

//     fn update_output(
//         &mut self,
//         _conn: &Connection,
//         _qh: &QueueHandle<Self>,
//         _output: wl_output::WlOutput,
//     ) {
//     }

//     fn output_destroyed(
//         &mut self,
//         _conn: &Connection,
//         _qh: &QueueHandle<Self>,
//         _output: wl_output::WlOutput,
//     ) {
//     }
// }

// impl LayerShellHandler for Wallpaper {
//     fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
//         self.exit = true;
//     }

//     fn configure(
//         &mut self,
//         _conn: &Connection,
//         qh: &QueueHandle<Self>,
//         _layer: &LayerSurface,
//         configure: LayerSurfaceConfigure,
//         _serial: u32,
//     ) {
//         if configure.new_size.0 == 0 || configure.new_size.1 == 0 {
//             self.width = 256;
//             self.height = 256;
//         } else {
//             self.width = configure.new_size.0;
//             self.height = configure.new_size.1;
//         }

//         // Initiate the first draw.
//         if self.first_configure {
//             self.first_configure = false;
//             self.draw(qh);
//         }
//     }
// }

// impl SeatHandler for Wallpaper {
//     fn seat_state(&mut self) -> &mut SeatState {
//         &mut self.seat_state
//     }

//     fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

//     fn new_capability(
//         &mut self,
//         _conn: &Connection,
//         qh: &QueueHandle<Self>,
//         seat: wl_seat::WlSeat,
//         capability: Capability,
//     ) {
//         if capability == Capability::Keyboard && self.keyboard.is_none() {
//             println!("Set keyboard capability");
//             let keyboard = self
//                 .seat_state
//                 .get_keyboard(qh, &seat, None)
//                 .expect("Failed to create keyboard");
//             self.keyboard = Some(keyboard);
//         }

//         if capability == Capability::Pointer && self.pointer.is_none() {
//             println!("Set pointer capability");
//             let pointer = self
//                 .seat_state
//                 .get_pointer(qh, &seat)
//                 .expect("Failed to create pointer");
//             self.pointer = Some(pointer);
//         }
//     }

//     fn remove_capability(
//         &mut self,
//         _conn: &Connection,
//         _: &QueueHandle<Self>,
//         _: wl_seat::WlSeat,
//         capability: Capability,
//     ) {
//         if capability == Capability::Keyboard && self.keyboard.is_some() {
//             println!("Unset keyboard capability");
//             self.keyboard.take().unwrap().release();
//         }

//         if capability == Capability::Pointer && self.pointer.is_some() {
//             println!("Unset pointer capability");
//             self.pointer.take().unwrap().release();
//         }
//     }

//     fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
// }

// impl KeyboardHandler for Wallpaper {
//     fn enter(
//         &mut self,
//         _: &Connection,
//         _: &QueueHandle<Self>,
//         _: &wl_keyboard::WlKeyboard,
//         surface: &wl_surface::WlSurface,
//         _: u32,
//         _: &[u32],
//         keysyms: &[u32],
//     ) {
//         if self.layer.wl_surface() == surface {
//             println!("Keyboard focus on window with pressed syms: {keysyms:?}");
//             self.keyboard_focus = true;
//         }
//     }

//     fn leave(
//         &mut self,
//         _: &Connection,
//         _: &QueueHandle<Self>,
//         _: &wl_keyboard::WlKeyboard,
//         surface: &wl_surface::WlSurface,
//         _: u32,
//     ) {
//         if self.layer.wl_surface() == surface {
//             println!("Release keyboard focus on window");
//             self.keyboard_focus = false;
//         }
//     }

//     fn press_key(
//         &mut self,
//         _conn: &Connection,
//         _qh: &QueueHandle<Self>,
//         _: &wl_keyboard::WlKeyboard,
//         _: u32,
//         event: KeyEvent,
//     ) {
//         println!("Key press: {event:?}");
//         // press 'esc' to exit
//         if event.keysym == keysyms::KEY_Escape {
//             self.exit = true;
//         }
//     }

//     fn release_key(
//         &mut self,
//         _: &Connection,
//         _: &QueueHandle<Self>,
//         _: &wl_keyboard::WlKeyboard,
//         _: u32,
//         event: KeyEvent,
//     ) {
//         println!("Key release: {event:?}");
//     }

//     fn update_modifiers(
//         &mut self,
//         _: &Connection,
//         _: &QueueHandle<Self>,
//         _: &wl_keyboard::WlKeyboard,
//         _serial: u32,
//         modifiers: Modifiers,
//     ) {
//         println!("Update modifiers: {modifiers:?}");
//     }
// }

// impl PointerHandler for Wallpaper {
//     fn pointer_frame(
//         &mut self,
//         _conn: &Connection,
//         _qh: &QueueHandle<Self>,
//         _pointer: &wl_pointer::WlPointer,
//         events: &[PointerEvent],
//     ) {
//         use PointerEventKind::*;
//         for event in events {
//             // Ignore events for other surfaces
//             if &event.surface != self.layer.wl_surface() {
//                 continue;
//             }
//             match event.kind {
//                 Enter { .. } => {
//                     println!("Pointer entered @{:?}", event.position);
//                 }
//                 Leave { .. } => {
//                     println!("Pointer left");
//                 }
//                 Motion { .. } => {}
//                 Press { button, .. } => {
//                     println!("Press {:x} @ {:?}", button, event.position);
//                     self.shift = self.shift.xor(Some(0));
//                 }
//                 Release { button, .. } => {
//                     println!("Release {:x} @ {:?}", button, event.position);
//                 }
//                 Axis {
//                     horizontal,
//                     vertical,
//                     ..
//                 } => {
//                     println!("Scroll H:{horizontal:?}, V:{vertical:?}");
//                 }
//             }
//         }
//     }
// }
delegate_compositor!(Wallpaper);
delegate_output!(Wallpaper);

delegate_seat!(Wallpaper);
delegate_keyboard!(Wallpaper);
delegate_pointer!(Wallpaper);

delegate_layer!(Wallpaper);

delegate_registry!(Wallpaper);

// impl ProvidesRegistryState for Wallpaper {
//     fn registry(&mut self) -> &mut RegistryState {
//         &mut self.registry_state
//     }
//     registry_handlers![OutputState, SeatState];
// }
