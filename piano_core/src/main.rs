use evdev::KeyCode;
use nix::{
    poll::PollTimeout,
    sys::epoll::{Epoll, EpollCreateFlags, EpollEvent, EpollFlags},
};
use std::{
    collections::HashMap,
    io::{BufWriter, Write},
    process::{ChildStdin, Command, Stdio},
    sync::mpsc::{Receiver, Sender},
    thread::{self},
};

#[derive(Debug, Clone)]
struct KeyBoardPair {
    keyboard_a: KeyBoard,
    keyboard_b: KeyBoard,
}

impl KeyBoardPair {
    pub fn init() -> Self {
        Self {
            keyboard_a: KeyBoard::init(0),
            keyboard_b: KeyBoard::init(1),
        }
    }
    pub fn get_midi_from_keycode(&self, key_code: &evdev::KeyCode) -> Option<&usize> {
        self.keyboard_a
            .keycode_midi_map
            .get(&key_code)
            .or_else(|| self.keyboard_b.keycode_midi_map.get(&key_code))
    }
}

#[derive(Debug, Clone)]
struct KeyBoard {
    // we need id to classified two different keyboards
    #[allow(dead_code)]
    id: usize,
    // a hashmap that store all the status of the keyboard
    keycode_midi_map: HashMap<evdev::KeyCode, usize>,
}

impl KeyBoard {
    pub fn init(id: usize) -> Self {
        Self {
            id: id,
            keycode_midi_map: HashMap::from([
                (KeyCode::KEY_1, 60),
                (KeyCode::KEY_2, 62),
                (KeyCode::KEY_3, 64),
                (KeyCode::KEY_4, 66),
                (KeyCode::KEY_5, 68),
                (KeyCode::KEY_6, 70),
                (KeyCode::KEY_7, 72),
            ]),
        }
    }
}

#[derive(Debug)]
enum KeyEvent {
    KeyPressed(KeyCode),
    KeyReleased(KeyCode),
}

fn pick_device() -> evdev::Device {
    println!("pick device!");
    // enumerate all the devices
    let devices = evdev::enumerate()
        .map(|device| device.1)
        .collect::<Vec<_>>();
    for (i, device) in devices.iter().enumerate() {
        println!("found NO.{} device:{}", i, device);
    }
    println!("please select your device [0-{}]", devices.len() - 1);
    let _ = std::io::stdout().flush(); // flush all stdout, cut noise
    let mut chosen = String::new();
    let _ = std::io::stdin().read_line(&mut chosen);
    let index = chosen.trim().parse::<usize>().unwrap();
    devices.into_iter().nth(index).unwrap()
}

fn listen_keyboards(
    tx: Sender<KeyEvent>,
    mut device: evdev::Device,
) -> anyhow::Result<(), anyhow::Error> {
    // set the device(like a kind of fd) as nonblocking
    let _ = device.set_nonblocking(true);

    // create epoll handle and attach raw_fd
    let epoll = Epoll::new(EpollCreateFlags::EPOLL_CLOEXEC).unwrap();
    let event = EpollEvent::new(EpollFlags::EPOLLIN, 0);
    epoll.add(&device, event).unwrap();

    // set the event to wait
    let mut events = [EpollEvent::empty(); 2];
    println!("Event:");
    loop {
        match device.fetch_events() {
            Ok(ev_iter) => {
                for ev in ev_iter {
                    match ev.event_type() {
                        evdev::EventType::KEY => {
                            match ev.value() {
                                0_i32 => {
                                    tx.send(KeyEvent::KeyReleased(evdev::KeyCode(ev.code())))?
                                }
                                1_i32 => {
                                    tx.send(KeyEvent::KeyPressed(evdev::KeyCode(ev.code())))?
                                }
                                2_i32 => {}
                                _ => {
                                    return Err(anyhow::Error::msg(
                                        "Unknown key value".to_string(),
                                    ));
                                }
                            };
                            println!("ev.value()={:?}", ev.value());
                            println!("ev.code()={:?}", ev.code());
                        }
                        _ => {}
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // wait forever for bytes available on dev
                epoll.wait(&mut events, PollTimeout::NONE).unwrap();
            }
            Err(e) => {
                return Err(anyhow::Error::msg(format!("err:{e}")));
            }
        }
    }
}

#[derive(Debug)]
enum NoteStatus {
    NoteOn,
    NoteOff,
}

fn play_note(
    stdin_buffer_write: &mut BufWriter<ChildStdin>,
    note_status: NoteStatus,
    channel: usize,
    midi: usize,
    velocity: usize,
) -> anyhow::Result<(), anyhow::Error> {
    match note_status {
        NoteStatus::NoteOn => {
            writeln!(
                stdin_buffer_write,
                "noteon {} {} {}",
                channel, midi, velocity
            )?;
            stdin_buffer_write.flush()?;
        }
        NoteStatus::NoteOff => {
            writeln!(
                stdin_buffer_write,
                "noteoff {} {} {}",
                channel, midi, velocity
            )?;
            stdin_buffer_write.flush()?;
        }
    };
    Ok(())
}

fn playing(
    rx: Receiver<KeyEvent>,
    mut synth: &mut BufWriter<ChildStdin>,
    my_keyboards: &KeyBoardPair,
) -> anyhow::Result<(), anyhow::Error> {
    loop {
        match rx.recv() {
            Ok(key_ev) => match key_ev {
                KeyEvent::KeyPressed(keycode) => {
                    let midi = match my_keyboards.get_midi_from_keycode(&keycode) {
                        Some(midi) => midi,
                        None => {
                            continue;
                        }
                    };
                    play_note(&mut synth, NoteStatus::NoteOn, 0, midi.clone(), 100)?;
                }
                KeyEvent::KeyReleased(keycode) => {
                    let midi = match my_keyboards.get_midi_from_keycode(&keycode) {
                        Some(midi) => midi,
                        None => {
                            continue;
                        }
                    };
                    play_note(&mut synth, NoteStatus::NoteOff, 0, midi.clone(), 100)?;
                }
            },
            Err(err) => {
                return Err(err.into());
            }
        }
    }
}

fn main() -> anyhow::Result<(), anyhow::Error> {
    // init a non-border channel
    let (tx, rx) = std::sync::mpsc::channel::<KeyEvent>();
    // // init a keyboard(virtual) Arc<Rwlock<_>>
    // let keyboard_a = Arc::new(RwLock::new(KeyBoard::init(0)));
    // pick the keyboard device
    let my_keyboards = KeyBoardPair::init();
    let device = pick_device();
    println!("picked the device:{}", device);

    // load extra bin 'fluidsynth'(https://github.com/fluidsynth/fluidsynth)
    let soundfont = "./TimGM6mb.sf2";
    let mut child = Command::new("fluidsynth")
        .args(["-a", "pipewire", "-n", soundfont])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdin = child
        .stdin
        .take()
        .expect("Cannot connect 'FluidSynth' 's stdin");
    let mut synth = BufWriter::new(stdin);

    play_note(&mut synth, NoteStatus::NoteOn, 0, 60, 100)?;

    let handle_keyboards = thread::spawn(move || {
        listen_keyboards(tx, device).unwrap();
    });

    let handle_playing = thread::spawn(move || {
        playing(rx, &mut synth, &my_keyboards).unwrap();
    });

    // wait for the thread to be done(break inner loop / return)
    handle_keyboards.join().unwrap();
    handle_playing.join().unwrap();
    // // quit the 'fluidsynth'
    // writeln!(synth, "quit")?;
    // synth.flush()?;
    // drop(synth);
    child.wait()?;
    Ok(())
}
