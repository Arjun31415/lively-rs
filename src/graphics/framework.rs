// taken from https://github.com/gfx-rs/wgpu/blob/trunk/examples/common/src/framework.rs
use input::event::pointer::PointerEvent as LibinputPointerEvent;
use input::{AsRaw, Libinput, LibinputInterface};
use nix::poll::{poll, PollFd, PollFlags};
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
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::os::fd::AsRawFd;
use std::os::unix::{fs::OpenOptionsExt, io::OwnedFd};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
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
pub static POINTER_POS: Mutex<(f64, f64)> = Mutex::new((0.0, 0.0));

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
    let handle = thread::spawn(|| {
        use std::process;
        println!("My pid is {}", process::id());
        track_mouse_movement();
        println!("Thread over");
    });
    println!("Starting event loop");

    loop {
        event_queue.blocking_dispatch(&mut w).unwrap();
        if w.exit {
            log::info!("Exiting");
            // TODO: destroy the thread handle
            break;
        }
    }
    handle.join();
}
struct Interface;

impl LibinputInterface for Interface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        OpenOptions::new()
            .custom_flags(flags)
            // Open as Read-Only, always
            .read(true)
            .write(false)
            .open(path)
            .map(|file| file.into())
            .map_err(|err| err.raw_os_error().unwrap())
    }
    fn close_restricted(&mut self, fd: OwnedFd) {
        drop(File::from(fd))
    }
}

fn track_mouse_movement() {
    let mut input = Libinput::new_with_udev(Interface);
    input.udev_assign_seat("seat0").unwrap();
    let pollfd = PollFd::new(input.as_raw_fd(), PollFlags::POLLIN);
    while poll(&mut [pollfd], -1).is_ok() {
        input.dispatch().unwrap();
        for event in &mut input {
            if let input::event::Event::Pointer(LibinputPointerEvent::Motion(pointer_event)) =
                &event
            {
                // println!("({}, {})", pointer_event.dx(), pointer_event.dy());
                // wait for lock
                let mut pos = POINTER_POS.lock().unwrap();
                (*pos).0 += pointer_event.dx();
                (*pos).1 += pointer_event.dy();
                drop(pos);
            }
        }
    }
    println!("returning from mouse");
}
delegate_compositor!(Wallpaper);
delegate_output!(Wallpaper);

delegate_seat!(Wallpaper);
delegate_keyboard!(Wallpaper);
delegate_pointer!(Wallpaper);

delegate_layer!(Wallpaper);

delegate_registry!(Wallpaper);
