use evdev::{AbsoluteAxisType, Device, InputEventKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use uinput::device::Device as UInputDevice;
use uinput::event::controller::Mouse;
use uinput::event::relative::Position;
use uinput::event::relative::Relative;
use uinput::event::controller::Controller;
use uinput::event::Event;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    device_name: String,

    sensitivity: f32,
    trackpad_to_pixel: f32,
    release_gain: f32,
    start_speed_min: f32,
    start_speed_max: f32,
    friction_per_16ms: f32,
    min_speed: f32,

    sample_window_ms: u64,
    pause_gap_ms: u64,
    pause_dist_threshold: f32,
    min_travel: f32,
    max_sample_staleness_ms: u64,

    grace_ms: u64,
    listener_poll_ms: u64,
    inertia_poll_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_name: "trackpad name".to_string(),
            sensitivity: 0.05,
            trackpad_to_pixel: 0.25,
            release_gain: 1.8,
            start_speed_min: 120.0,
            start_speed_max: 7000.0,
            friction_per_16ms: 0.965,
            min_speed: 25.0,
            sample_window_ms: 50,
            pause_gap_ms: 28,
            pause_dist_threshold: 0.8,
            min_travel: 8.0,
            max_sample_staleness_ms: 120,
            grace_ms: 80,
            listener_poll_ms: 2,
            inertia_poll_ms: 2,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct SlotState {
    active: bool,
    x: Option<f32>,
    y: Option<f32>,
}

#[derive(Clone, Copy)]
struct Sample {
    x: f32,
    y: f32,
    t: Instant,
}

#[derive(Clone, Copy, Default)]
struct InertiaState {
    active: bool,
    vx: f32,
    vy: f32,
    acc_x: f32,
    acc_y: f32,
}

struct SharedState {
    slots: HashMap<i32, SlotState>,
    finger_count: usize,
    prev_finger_count: usize,
    samples: VecDeque<Sample>,
    suppress_until: Option<Instant>,
    inertia: InertiaState,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Arc::new(load_or_init_config()?);
    println!("using config:\n{:#?}", cfg);

    let device_path = find_event_device_by_name(&cfg.device_name)?;
    println!("trackpad device path: {}", device_path.display());

    let mut device = Device::open(&device_path)?;
    println!("listening to device: {:?}", device.name());

    let mut ui: UInputDevice = uinput::default()?
        .name("inertia virtual mouse")?
        .event(Event::Controller(Controller::Mouse(Mouse::Left)))? //necessary for some reason
        .event(Event::Relative(Relative::Position(Position::X)))?
        .event(Event::Relative(Relative::Position(Position::Y)))?
        .create()?;

    println!("virtual mouse ready.");
    thread::sleep(Duration::from_secs(1));

    let state = Arc::new(Mutex::new(SharedState {
        slots: HashMap::new(),
        finger_count: 0,
        prev_finger_count: 0,
        samples: VecDeque::with_capacity(64),
        suppress_until: None,
        inertia: InertiaState::default(),
    }));

    {
        let state = Arc::clone(&state);
        let cfg = Arc::clone(&cfg);

        thread::spawn(move || {
            let mut current_slot: i32 = 0;

            loop {
                let events = match device.fetch_events() {
                    Ok(events) => events,
                    Err(_) => {
                        thread::sleep(Duration::from_millis(cfg.listener_poll_ms));
                        continue;
                    }
                };

                for ev in events {
                    match ev.kind() {
                        InputEventKind::AbsAxis(axis) => match axis {
                            AbsoluteAxisType::ABS_MT_SLOT => {
                                current_slot = ev.value();
                                let mut st = state.lock().unwrap();
                                st.slots.entry(current_slot).or_default();
                            }
                            AbsoluteAxisType::ABS_MT_TRACKING_ID => {
                                let mut st = state.lock().unwrap();
                                let slot = st.slots.entry(current_slot).or_default();

                                if ev.value() >= 0 {
                                    slot.active = true;
                                } else {
                                    slot.active = false;
                                    slot.x = None;
                                    slot.y = None;
                                }
                            }
                            AbsoluteAxisType::ABS_MT_POSITION_X | AbsoluteAxisType::ABS_X => {
                                let mut st = state.lock().unwrap();
                                let slot = st.slots.entry(current_slot).or_default();
                                slot.x = Some(ev.value() as f32);
                            }
                            AbsoluteAxisType::ABS_MT_POSITION_Y | AbsoluteAxisType::ABS_Y => {
                                let mut st = state.lock().unwrap();
                                let slot = st.slots.entry(current_slot).or_default();
                                slot.y = Some(ev.value() as f32);
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }

                let now = Instant::now();
                let mut st = state.lock().unwrap();

                st.prev_finger_count = st.finger_count;
                st.finger_count = st
                    .slots
                    .values()
                    .filter(|s| s.active && s.x.is_some() && s.y.is_some())
                    .count();

                let prev = st.prev_finger_count;
                let count = st.finger_count;

                if count > 0 {
                    st.inertia.active = false;
                    st.inertia.vx = 0.0;
                    st.inertia.vy = 0.0;
                    st.inertia.acc_x = 0.0;
                    st.inertia.acc_y = 0.0;
                }

                if prev == 0 && count > 0 {
                    st.samples.clear();
                    st.suppress_until = None;
                }

                // keep gestures usable
                if prev > 1 && count == 1 {
                    st.inertia.active = false;
                    st.inertia.vx = 0.0;
                    st.inertia.vy = 0.0;
                    st.inertia.acc_x = 0.0;
                    st.inertia.acc_y = 0.0;
                    st.samples.clear();
                    st.suppress_until = Some(now + Duration::from_millis(cfg.grace_ms));
                }

                if count == 1 {
                    let suppressed = st.suppress_until.map_or(false, |t| now < t);
                    if !suppressed {
                        if let Some(pos) = sole_position(&st.slots) {
                            let push = match st.samples.back() {
                                Some(last) => last.x != pos.0 || last.y != pos.1,
                                None => true,
                            };

                            if push {
                                st.samples.push_back(Sample {
                                    x: pos.0,
                                    y: pos.1,
                                    t: now,
                                });
                                prune_samples(&mut st.samples, now, cfg.sample_window_ms);
                            }
                        }
                    }
                }

                if prev == 1 && count == 0 {
                    let blocked = st.suppress_until.map_or(false, |t| now < t);

                    // grace period
                    if blocked {
                        st.inertia.active = false;
                        st.inertia.vx = 0.0;
                        st.inertia.vy = 0.0;
                        st.inertia.acc_x = 0.0;
                        st.inertia.acc_y = 0.0;
                        st.samples.clear();
                    } else if let Some((vx, vy)) = estimate_release_velocity(&st.samples, now, &cfg)
                    {
                        st.inertia.active = true;
                        st.inertia.vx = vx;
                        st.inertia.vy = vy;
                        st.inertia.acc_x = 0.0;
                        st.inertia.acc_y = 0.0;
                        //println!("starting inertia: vx={:.1}px/s vy={:.1}px/s", st.inertia.vx, st.inertia.vy) //debug
                    } else {
                        st.inertia.active = false;
                        st.inertia.vx = 0.0;
                        st.inertia.vy = 0.0;
                        st.inertia.acc_x = 0.0;
                        st.inertia.acc_y = 0.0;
                    }
                }

                thread::sleep(Duration::from_millis(cfg.listener_poll_ms));
            }
        });
    }

    let mut last_tick = Instant::now();

    loop {
        let now = Instant::now();
        let dt = now.duration_since(last_tick);
        last_tick = now;
        let dt_secs = dt.as_secs_f32().max(0.001);

        let mut step_x = 0i32;
        let mut step_y = 0i32;
        let mut do_move = false;

        {
            let mut st = state.lock().unwrap();

            if st.finger_count > 0 {
                st.inertia.active = false;
                st.inertia.vx = 0.0;
                st.inertia.vy = 0.0;
                st.inertia.acc_x = 0.0;
                st.inertia.acc_y = 0.0;
            } else if st.inertia.active {
                if st.suppress_until.map_or(false, |t| now < t) {
                    st.inertia.active = false;
                    st.inertia.vx = 0.0;
                    st.inertia.vy = 0.0;
                    st.inertia.acc_x = 0.0;
                    st.inertia.acc_y = 0.0;
                } else if st.inertia.vx.hypot(st.inertia.vy) >= cfg.min_speed {
                    do_move = true;

                    st.inertia.acc_x += st.inertia.vx * dt_secs;
                    st.inertia.acc_y += st.inertia.vy * dt_secs;

                    step_x = st.inertia.acc_x.trunc() as i32;
                    step_y = st.inertia.acc_y.trunc() as i32;

                    st.inertia.acc_x -= step_x as f32;
                    st.inertia.acc_y -= step_y as f32;

                    let decay = cfg.friction_per_16ms.powf(dt_secs / 0.016_f32);
                    st.inertia.vx *= decay;
                    st.inertia.vy *= decay;

                    if st.inertia.vx.hypot(st.inertia.vy) < cfg.min_speed {
                        st.inertia.active = false;
                        st.inertia.acc_x = 0.0;
                        st.inertia.acc_y = 0.0;
                    }
                } else {
                    st.inertia.active = false;
                    st.inertia.acc_x = 0.0;
                    st.inertia.acc_y = 0.0;
                }
            }
        }

        if do_move {
            if step_x != 0 {
                let dir = step_x.signum();
                for _ in 0..step_x.abs() {
                    ui.send(Event::Relative(Relative::Position(Position::X)), dir)?;
                }
            }

            if step_y != 0 {
                let dir = step_y.signum();
                for _ in 0..step_y.abs() {
                    ui.send(Event::Relative(Relative::Position(Position::Y)), dir)?;
                }
            }

            ui.synchronize()?;
        }

        thread::sleep(Duration::from_millis(cfg.inertia_poll_ms));
    }
}

fn config_path() -> PathBuf {
    PathBuf::from("/etc/inertia/config.toml")
}

fn load_or_init_config() -> Result<Config, Box<dyn std::error::Error>> {
    let path = config_path();

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    if !path.exists() {
        let default_cfg = Config::default();
        let text = toml::to_string_pretty(&default_cfg)?;
        fs::write(&path, text)?;
        println!("wrote default config to {}", path.display());
        return Ok(default_cfg);
    }

    let text = fs::read_to_string(&path)?;
    let cfg: Config = toml::from_str(&text)?;
    Ok(cfg)
}

fn find_event_device_by_name(device_name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let content = fs::read_to_string("/proc/bus/input/devices")?;

    for block in content.split("\n\n") {
        let mut name: Option<String> = None;
        let mut handlers: Option<Vec<String>> = None;

        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("N: Name=\"") {
                let name_str = rest.trim_end_matches('"').to_string();
                name = Some(name_str);
            } else if let Some(rest) = line.strip_prefix("H: Handlers=") {
                let hs = rest
                    .split_whitespace()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>();
                handlers = Some(hs);
            }
        }

        if let (Some(n), Some(hs)) = (name, handlers) {
            if n == device_name {
                if let Some(event_handler) = hs.into_iter().find(|h| h.starts_with("event")) {
                    return Ok(PathBuf::from(format!("/dev/input/{}", event_handler)));
                }
            }
        }
    }

    Err(format!(
        "No input device named {:?} found in /proc/bus/input/devices",
        device_name
    )
    .into())
}

fn sole_position(slots: &HashMap<i32, SlotState>) -> Option<(f32, f32)> {
    let mut found: Option<(f32, f32)> = None;

    for slot in slots.values() {
        if slot.active {
            let pos = match (slot.x, slot.y) {
                (Some(x), Some(y)) => (x, y),
                _ => continue,
            };

            if found.is_some() {
                return None;
            }
            found = Some(pos);
        }
    }

    found
}

fn prune_samples(samples: &mut VecDeque<Sample>, now: Instant, sample_window_ms: u64) {
    while let Some(front) = samples.front() {
        if now.duration_since(front.t).as_millis() as u64 > sample_window_ms {
            samples.pop_front();
        } else {
            break;
        }
    }
}

fn estimate_release_velocity(
    samples: &VecDeque<Sample>,
    release_time: Instant,
    cfg: &Config,
) -> Option<(f32, f32)> {
    if samples.len() < 3 {
        return None;
    }

    let newest = *samples.back()?;

    let age_ms = release_time.duration_since(newest.t).as_secs_f32() * 1000.0;
    // it was really unwieldy without this
    let confidence = (1.0 - (age_ms / cfg.max_sample_staleness_ms as f32)).clamp(0.0, 1.0);
    if confidence <= 0.05 {
        return None;
    }

    let mut segment: Vec<Sample> = vec![newest];
    let mut last = newest;
    let mut total_travel = 0.0;

    for &s in samples.iter().rev().skip(1) {
        let span_ms = newest.t.duration_since(s.t).as_millis() as u64;
        if span_ms > cfg.sample_window_ms {
            break;
        }

        let dt_ms = last.t.duration_since(s.t).as_millis() as u64;
        let dist = ((last.x - s.x).powi(2) + (last.y - s.y).powi(2)).sqrt();

        if dt_ms > cfg.pause_gap_ms
            && dist < cfg.pause_dist_threshold
            && total_travel >= cfg.min_travel
        {
            break;
        }

        segment.push(s);
        total_travel += dist;
        last = s;
    }

    if segment.len() < 3 || total_travel < cfg.min_travel {
        return None;
    }

    segment.reverse();

    let (mut vx, mut vy) = weighted_velocity(&segment)?;
    vx *= confidence;
    vy *= confidence;

    let mag = vx.hypot(vy);
    if mag < 1.0 {
        return None;
    }

    let target_mag = (mag * cfg.trackpad_to_pixel * cfg.release_gain)
        .clamp(cfg.start_speed_min, cfg.start_speed_max);

    let scale = target_mag / mag;
    Some((vx * scale, vy * scale))
}

fn weighted_velocity(samples: &[Sample]) -> Option<(f32, f32)> {
    if samples.len() < 2 {
        return None;
    }

    let t0 = samples[0].t;

    let mut sw = 0.0f32;
    let mut st = 0.0f32;
    let mut sx = 0.0f32;
    let mut sy = 0.0f32;
    let mut stt = 0.0f32;
    let mut stx = 0.0f32;
    let mut sty = 0.0f32;

    for (i, s) in samples.iter().enumerate() {
        let w = 1.0 + i as f32;
        let t = s.t.duration_since(t0).as_secs_f32();

        sw += w;
        st += w * t;
        sx += w * s.x;
        sy += w * s.y;
        stt += w * t * t;
        stx += w * t * s.x;
        sty += w * t * s.y;
    }

    let denom = sw * stt - st * st;
    if denom.abs() < 1e-6 {
        return None;
    }

    let slope_x = (sw * stx - st * sx) / denom;
    let slope_y = (sw * sty - st * sy) / denom;

    Some((slope_x, slope_y))
}
