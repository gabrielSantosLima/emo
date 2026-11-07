//! Desktop frontend for `emo`: window, audio and keyboard input.
//!
//! The frontend only owns presentation and pacing. Emulation stays in
//! `emo-core`; the loop advances whole frames and never reaches into the PPU.

use std::collections::{HashSet, VecDeque};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use emo_core::{Cartridge, Machine, VideoStandard};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

/// APU output rate: the S-DSP sample clock divided by 32.
const AUDIO_RATE: u32 = 32_000;

/// Controller button bits (active high).
const BUTTON_B: u16 = 0x8000;
const BUTTON_Y: u16 = 0x4000;
const BUTTON_SELECT: u16 = 0x2000;
const BUTTON_START: u16 = 0x1000;
const BUTTON_UP: u16 = 0x0800;
const BUTTON_DOWN: u16 = 0x0400;
const BUTTON_LEFT: u16 = 0x0200;
const BUTTON_RIGHT: u16 = 0x0100;
const BUTTON_A: u16 = 0x0080;
const BUTTON_X: u16 = 0x0040;
const BUTTON_L: u16 = 0x0020;
const BUTTON_R: u16 = 0x0010;

/// Rewind snapshots kept in the ring buffer (about six seconds at 60 Hz with
/// one snapshot every 30 frames).
const REWIND_STATES: usize = 120;
/// Frames between rewind snapshots.
const REWIND_INTERVAL: u32 = 30;
/// Frames per iteration while fast-forwarding.
const FAST_FORWARD_FRAMES: u32 = 8;

fn usage() -> ExitCode {
    eprintln!(
        "usage: emo-desktop <rom> [--ipl <path>] [--sram <path>] [--scale <n>]\n\
         \n\
         Keys: arrows = D-pad, Z/X = B/A, A/S = Y/X, Q/W = L/R,\n\
         Enter = Start, Right Shift = Select\n\
         P = pause, Tab = fast-forward, Backspace = rewind,\n\
         F5 = save state, F8 = load state, Esc = quit"
    );
    ExitCode::FAILURE
}

struct Args {
    rom: PathBuf,
    ipl: Option<PathBuf>,
    sram: Option<PathBuf>,
    scale: u32,
}

fn parse_args() -> Result<Args, ExitCode> {
    let mut rom = None;
    let mut ipl = None;
    let mut sram = None;
    let mut scale = 0;

    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let value = arg.to_string_lossy().into_owned();
        match value.as_str() {
            "--ipl" | "--sram" | "--scale" => {
                let Some(next) = args.next() else {
                    eprintln!("missing value after {value}");
                    return Err(usage());
                };
                match value.as_str() {
                    "--ipl" => ipl = Some(PathBuf::from(next)),
                    "--sram" => sram = Some(PathBuf::from(next)),
                    _ => {
                        scale = next.to_string_lossy().parse().unwrap_or(0);
                        if scale == 0 {
                            eprintln!("invalid --scale value");
                            return Err(usage());
                        }
                    }
                }
            }
            _ if rom.is_none() && !value.starts_with("--") => rom = Some(PathBuf::from(arg)),
            _ => {
                eprintln!("unknown argument {value:?}");
                return Err(usage());
            }
        }
    }

    let Some(rom) = rom else {
        return Err(usage());
    };
    Ok(Args {
        rom,
        ipl,
        sram,
        scale,
    })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(code) => return code,
    };

    let machine = match build_machine(&args) {
        Ok(machine) => machine,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };

    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("failed to create the event loop: {error}");
            return ExitCode::FAILURE;
        }
    };

    let state_path = state_path(&args.rom);
    let mut app = App {
        machine,
        state_path,
        requested_scale: args.scale,
        window: None,
        _context: None,
        surface: None,
        audio: None,
        audio_phase: 0.0,
        held: HashSet::new(),
        paused: false,
        fast_forward: false,
        rewinding: false,
        rewind: VecDeque::new(),
        frames_until_snapshot: REWIND_INTERVAL,
        next_frame: Instant::now(),
        fps_start: Instant::now(),
        fps_frames: 0,
        fps: 0.0,
        scale_text: 0,
    };

    if let Err(error) = event_loop.run_app(&mut app) {
        eprintln!("event loop error: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn build_machine(args: &Args) -> Result<Machine, String> {
    let bytes = std::fs::read(&args.rom)
        .map_err(|error| format!("failed to read {}: {error}", args.rom.display()))?;
    let mut cartridge = Cartridge::from_rom(bytes).map_err(|error| error.to_string())?;

    if let Some(sram) = &args.sram
        && let Ok(data) = std::fs::read(sram)
    {
        cartridge.load_sram(&data);
    }

    let mut machine = Machine::new(cartridge).map_err(|error| error.to_string())?;

    let ipl = args.ipl.clone().or_else(|| {
        ["roms/ipl.bin", "ipl.bin"]
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
    });
    if let Some(ipl) = ipl {
        let bytes = std::fs::read(&ipl)
            .map_err(|error| format!("failed to read {}: {error}", ipl.display()))?;
        machine.memory_mut().apu_mut().load_ipl(&bytes);
        eprintln!("using IPL ROM {}", ipl.display());
    } else {
        eprintln!(
            "no IPL ROM found; using the experimental built-in boot protocol (place a 64-byte ipl.bin in roms/ to use the real one)"
        );
    }

    Ok(machine)
}

fn state_path(rom: &Path) -> PathBuf {
    let mut path = rom.to_path_buf();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "emo".to_owned());
    path.set_file_name(format!("{name}.state"));
    path
}

/// Stereo audio queue feeding the output device.
struct Audio {
    buffer: Arc<Mutex<VecDeque<f32>>>,
    rate: u32,
    _stream: cpal::Stream,
}

impl Audio {
    fn new() -> Option<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device()?;
        let supported = device.default_output_config().ok()?;
        let rate = supported.sample_rate().0;
        let config: cpal::StreamConfig = supported.config();
        let buffer = Arc::new(Mutex::new(VecDeque::new()));

        let sink = Arc::clone(&buffer);
        let error = |error| eprintln!("audio stream error: {error}");
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config,
                move |data: &mut [f32], _| {
                    let mut queue = match sink.lock() {
                        Ok(queue) => queue,
                        Err(_) => return,
                    };
                    let mut last = 0.0;
                    for sample in data.iter_mut() {
                        if let Some(value) = queue.pop_front() {
                            last = value;
                        }
                        *sample = last;
                    }
                },
                error,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config,
                move |data: &mut [i16], _| {
                    let mut queue = match sink.lock() {
                        Ok(queue) => queue,
                        Err(_) => return,
                    };
                    let mut last = 0.0;
                    for sample in data.iter_mut() {
                        if let Some(value) = queue.pop_front() {
                            last = value;
                        }
                        *sample = (last * f32::from(i16::MAX)) as i16;
                    }
                },
                error,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_output_stream(
                &config,
                move |data: &mut [u16], _| {
                    let mut queue = match sink.lock() {
                        Ok(queue) => queue,
                        Err(_) => return,
                    };
                    let mut last = 0.0;
                    for sample in data.iter_mut() {
                        if let Some(value) = queue.pop_front() {
                            last = value;
                        }
                        *sample = ((last * 0.5 + 0.5) * f32::from(u16::MAX)) as u16;
                    }
                },
                error,
                None,
            ),
            _ => return None,
        };

        let stream = stream.ok()?;
        stream.play().ok()?;
        Some(Self {
            buffer,
            rate,
            _stream: stream,
        })
    }

    /// Resample one frame of interleaved stereo samples to the device rate.
    fn push(&self, samples: &[i16], phase: &mut f32) {
        let mut queue = match self.buffer.lock() {
            Ok(queue) => queue,
            Err(_) => return,
        };
        let frames = samples.len() / 2;
        if frames == 0 {
            return;
        }
        let step = AUDIO_RATE as f32 / self.rate as f32;
        // Keep at most ~100 ms of backlog so the output never lags the picture.
        let capacity = (self.rate / 10) as usize * 2;
        while queue.len() > capacity {
            queue.pop_front();
        }

        loop {
            let index = *phase as usize;
            if index + 1 >= frames {
                *phase -= frames as f32;
                break;
            }
            let fraction = *phase - index as f32;
            for channel in 0..2 {
                let a = f32::from(samples[index * 2 + channel]) / f32::from(i16::MAX);
                let b = f32::from(samples[(index + 1) * 2 + channel]) / f32::from(i16::MAX);
                queue.push_back(a + (b - a) * fraction);
            }
            *phase += step;
        }
    }
}

struct App {
    machine: Machine,
    state_path: PathBuf,
    requested_scale: u32,
    window: Option<Arc<Window>>,
    _context: Option<softbuffer::Context<Arc<Window>>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    audio: Option<Audio>,
    audio_phase: f32,
    held: HashSet<KeyCode>,
    paused: bool,
    fast_forward: bool,
    rewinding: bool,
    rewind: VecDeque<Vec<u8>>,
    frames_until_snapshot: u32,
    next_frame: Instant,
    fps_start: Instant,
    fps_frames: u32,
    fps: f32,
    scale_text: u32,
}

impl App {
    fn update_buttons(&mut self) {
        let mut buttons = 0;
        for key in &self.held {
            buttons |= match key {
                KeyCode::ArrowUp => BUTTON_UP,
                KeyCode::ArrowDown => BUTTON_DOWN,
                KeyCode::ArrowLeft => BUTTON_LEFT,
                KeyCode::ArrowRight => BUTTON_RIGHT,
                KeyCode::KeyZ => BUTTON_B,
                KeyCode::KeyX => BUTTON_A,
                KeyCode::KeyA => BUTTON_Y,
                KeyCode::KeyS => BUTTON_X,
                KeyCode::KeyQ => BUTTON_L,
                KeyCode::KeyW => BUTTON_R,
                KeyCode::Enter => BUTTON_START,
                KeyCode::ShiftRight => BUTTON_SELECT,
                _ => 0,
            };
        }
        self.machine.set_buttons(0, buttons);
    }

    fn run_frame(&mut self) {
        self.machine.run_frame();

        let samples = self.machine.take_audio();
        if let Some(audio) = &self.audio {
            audio.push(&samples, &mut self.audio_phase);
        }

        self.fps_frames += 1;
        let elapsed = self.fps_start.elapsed();
        if elapsed >= Duration::from_secs(1) {
            self.fps = self.fps_frames as f32 / elapsed.as_secs_f32();
            self.fps_frames = 0;
            self.fps_start = Instant::now();
            self.update_title();
        }

        let interval = self.machine.video_standard().frame_period();
        self.next_frame += interval;
        let now = Instant::now();
        if self.next_frame < now - interval * 4 {
            // Do not accumulate debt after a stall (debugger, window drag).
            self.next_frame = now;
        }
    }

    fn snapshot(&mut self) {
        self.frames_until_snapshot = REWIND_INTERVAL;
        if let Ok(state) = self.machine.save_state() {
            if self.rewind.len() == REWIND_STATES {
                self.rewind.pop_front();
            }
            self.rewind.push_back(state);
        }
    }

    fn rewind_frame(&mut self) {
        let Some(state) = self.rewind.pop_back() else {
            self.rewinding = false;
            return;
        };
        if self.machine.load_state(&state).is_err() {
            self.rewind.clear();
            self.rewinding = false;
        }
    }

    fn save_state(&mut self) {
        match self.machine.save_state() {
            Ok(state) => match std::fs::write(&self.state_path, state) {
                Ok(()) => eprintln!("saved state to {}", self.state_path.display()),
                Err(error) => eprintln!("failed to write state: {error}"),
            },
            Err(error) => eprintln!("failed to serialize state: {error}"),
        }
    }

    fn load_state(&mut self) {
        match std::fs::read(&self.state_path) {
            Ok(state) => match self.machine.load_state(&state) {
                Ok(()) => {
                    self.rewind.clear();
                    eprintln!("loaded state from {}", self.state_path.display());
                }
                Err(error) => eprintln!("failed to load state: {error}"),
            },
            Err(error) => eprintln!("failed to read state: {error}"),
        }
    }

    fn update_title(&self) {
        if let Some(window) = &self.window {
            let flags = match (self.paused, self.fast_forward, self.rewinding) {
                (true, _, _) => " [paused]",
                (_, true, _) => " [fast]",
                (_, _, true) => " [rewind]",
                _ => "",
            };
            window.set_title(&format!(
                "emo - {:.1} fps{} - {}x",
                self.fps, flags, self.scale_text
            ));
        }
    }

    fn draw(&mut self) {
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
            return;
        };
        let size = window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        if surface.resize(width, height).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };

        let frame_width = 256usize;
        let frame_height = usize::from(self.machine.visible_lines());
        let scale = if self.requested_scale > 0 {
            self.requested_scale as usize
        } else {
            (size.width as usize / frame_width)
                .min(size.height as usize / frame_height)
                .max(1)
        };
        self.scale_text = scale as u32;

        let output_width = size.width as usize;
        let output_height = size.height as usize;
        buffer.fill(0);
        let frame = self.machine.framebuffer();
        let offset_x = (output_width.saturating_sub(frame_width * scale)) / 2;
        let offset_y = (output_height.saturating_sub(frame_height * scale)) / 2;

        for y in 0..frame_height {
            for x in 0..frame_width {
                let source = (y * frame_width + x) * 3;
                let color = (u32::from(frame[source]) << 16)
                    | (u32::from(frame[source + 1]) << 8)
                    | u32::from(frame[source + 2]);
                for dy in 0..scale {
                    let row = offset_y + y * scale + dy;
                    if row >= output_height {
                        continue;
                    }
                    let len = buffer.len();
                    let start = (row * output_width + offset_x + x * scale).min(len);
                    let end = (start + scale).min((row + 1) * output_width).min(len);
                    buffer[start..end].fill(color);
                }
            }
        }

        if let Err(error) = buffer.present() {
            eprintln!("failed to present the frame: {error}");
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("emo")
            .with_inner_size(winit::dpi::LogicalSize::new(768, 672));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("failed to create the window: {error}");
                event_loop.exit();
                return;
            }
        };

        let context = match softbuffer::Context::new(window.clone()) {
            Ok(context) => context,
            Err(error) => {
                eprintln!("failed to create the display context: {error}");
                event_loop.exit();
                return;
            }
        };
        let surface = match softbuffer::Surface::new(&context, window.clone()) {
            Ok(surface) => surface,
            Err(error) => {
                eprintln!("failed to create the window surface: {error}");
                event_loop.exit();
                return;
            }
        };

        self.audio = Audio::new();
        if let Some(audio) = &self.audio
            && let Ok(mut queue) = audio.buffer.lock()
        {
            // A short lead-in keeps the callback from starving while the window
            // loop is still settling; a longer one is heard as input lag.
            queue.extend(std::iter::repeat_n(0.0, (audio.rate / 100) as usize));
        }
        if self.audio.is_none() {
            eprintln!("no audio output device; running without sound");
        }

        self.window = Some(window);
        self._context = Some(context);
        self.surface = Some(surface);
        self.fps_start = Instant::now();
        self.next_frame = Instant::now();
        self.update_title();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state,
                        repeat,
                        ..
                    },
                ..
            } => {
                let pressed = state == ElementState::Pressed;
                if pressed && !repeat {
                    match code {
                        KeyCode::Escape => {
                            event_loop.exit();
                            return;
                        }
                        KeyCode::KeyP => {
                            self.paused = !self.paused;
                            self.update_title();
                        }
                        KeyCode::F5 => self.save_state(),
                        KeyCode::F8 => self.load_state(),
                        _ => {}
                    }
                }

                if pressed {
                    self.held.insert(code);
                } else {
                    self.held.remove(&code);
                }
                self.fast_forward = self.held.contains(&KeyCode::Tab);
                self.rewinding = self.held.contains(&KeyCode::Backspace);
                self.update_buttons();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let interval = self.machine.video_standard().frame_period();
        let now = Instant::now();

        if self.paused {
            event_loop.set_control_flow(ControlFlow::WaitUntil(now + interval));
            return;
        }

        // Only advance the machine when the frame deadline is reached. Waking
        // up for a redraw must never run extra frames, otherwise the emulator
        // spins at whatever rate the window system delivers paint messages.
        if now >= self.next_frame {
            if self.rewinding && !self.rewind.is_empty() {
                self.rewind_frame();
            } else {
                let frames = if self.fast_forward {
                    FAST_FORWARD_FRAMES
                } else {
                    1
                };
                for _ in 0..frames {
                    self.run_frame();
                    self.frames_until_snapshot = self.frames_until_snapshot.saturating_sub(1);
                    if self.frames_until_snapshot == 0 {
                        self.snapshot();
                    }
                }
            }

            if let Some(window) = &self.window {
                window.request_redraw();
            }
            if self.next_frame <= Instant::now() {
                self.next_frame = Instant::now() + interval;
            }
        }

        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

/// Frame period of the video standard (NTSC 60.0988 Hz).
trait FramePeriod {
    fn frame_period(self) -> Duration;
}

impl FramePeriod for VideoStandard {
    fn frame_period(self) -> Duration {
        Duration::from_secs_f64(1.0 / 60.0988)
    }
}
