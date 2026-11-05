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

#[derive(Debug, Clone, Copy)]
enum KeyBoardID {
    KeyBoardLeft,
    KeyBoardRight,
}

#[derive(Debug)]
enum KeyEvent {
    KeyPressed((KeyBoardID, KeyCode)),
    KeyReleased((KeyBoardID, KeyCode)),
}

#[derive(Debug, Clone)]
struct KeyBoardPair {
    keyboard_left: KeyBoard,
    keyboard_right: KeyBoard,
}

impl KeyBoardPair {
    pub fn init() -> Self {
        Self {
            keyboard_left: KeyBoard::init(KeyBoardID::KeyBoardLeft),
            keyboard_right: KeyBoard::init(KeyBoardID::KeyBoardRight),
        }
    }
    pub fn get_midi(&self, keyboard_id: &KeyBoardID, key_code: &KeyCode) -> Option<&usize> {
        match keyboard_id {
            KeyBoardID::KeyBoardLeft => self.keyboard_left.keycode_midi_map.get(&key_code),
            KeyBoardID::KeyBoardRight => self.keyboard_right.keycode_midi_map.get(&key_code),
        }
    }
}

#[derive(Debug, Clone)]
struct KeyBoard {
    // we need id to classified two different keyboards
    #[allow(dead_code)]
    id: KeyBoardID,
    // a hashmap that store all the status of the keyboard
    keycode_midi_map: HashMap<evdev::KeyCode, usize>,
}

impl KeyBoard {
    pub fn init(id: KeyBoardID) -> Self {
        let mut midi_map = HashMap::from([
            // -----------1-------------
            (KeyCode::KEY_1, 24),  // C1
            (KeyCode::KEY_F1, 25), // C♯1
            (KeyCode::KEY_2, 26),  // D1
            (KeyCode::KEY_F2, 27), // D#1
            (KeyCode::KEY_3, 28),  // E1
            (KeyCode::KEY_4, 29),  // F1
            (KeyCode::KEY_F3, 30), // F#1
            (KeyCode::KEY_5, 31),  // G1
            (KeyCode::KEY_8, 32),  // G#1
            (KeyCode::KEY_6, 33),  // A1
            (KeyCode::KEY_9, 34),  // A#1
            (KeyCode::KEY_7, 35),  // B1
            // -----------2-------------
            (KeyCode::KEY_Q, 36),  // C2
            (KeyCode::KEY_F4, 37), // C♯2
            (KeyCode::KEY_W, 38),  // D2
            (KeyCode::KEY_F5, 39), // D#2
            (KeyCode::KEY_E, 40),  // E2
            (KeyCode::KEY_R, 41),  // F2
            (KeyCode::KEY_F6, 42), // F#2
            (KeyCode::KEY_T, 43),  // G2
            (KeyCode::KEY_I, 44),  // G#2
            (KeyCode::KEY_Y, 45),  // A2
            (KeyCode::KEY_O, 46),  // A#2
            (KeyCode::KEY_U, 47),  // B2
            // -----------3-------------
            (KeyCode::KEY_A, 48),  // C3
            (KeyCode::KEY_F7, 49), // C♯3
            (KeyCode::KEY_S, 50),  // D3
            (KeyCode::KEY_F8, 51), // D#3
            (KeyCode::KEY_D, 52),  // E3
            (KeyCode::KEY_F, 53),  // F3
            (KeyCode::KEY_F9, 54), // F#3
            (KeyCode::KEY_G, 55),  // G3
            (KeyCode::KEY_K, 56),  // G#3
            (KeyCode::KEY_H, 57),  // A3
            (KeyCode::KEY_L, 58),  // A#3
            (KeyCode::KEY_J, 59),  // B3
            // -----------4-------------
            (KeyCode::KEY_Z, 60),     // C4
            (KeyCode::KEY_F10, 61),   // C♯4
            (KeyCode::KEY_X, 62),     // D4
            (KeyCode::KEY_F11, 63),   // D#4
            (KeyCode::KEY_C, 64),     // E4
            (KeyCode::KEY_V, 65),     // F4
            (KeyCode::KEY_F12, 66),   // F#4
            (KeyCode::KEY_B, 67),     // G4
            (KeyCode::KEY_COMMA, 68), // G#4
            (KeyCode::KEY_N, 69),     // A4
            (KeyCode::KEY_DOT, 70),   // A#4
            (KeyCode::KEY_M, 71),     // B4
        ]);
        match id {
            KeyBoardID::KeyBoardLeft => Self {
                id,
                keycode_midi_map: midi_map,
            },
            KeyBoardID::KeyBoardRight => Self {
                id,
                keycode_midi_map: {
                    midi_map.values_mut().for_each(|midi| *midi += 48usize);
                    midi_map.retain(|_key, val| *val <= 107);
                    midi_map
                },
            },
        }
    }
}

fn pick_device() -> evdev::Device {
    // enumerate all the devices
    let devices = evdev::enumerate()
        .map(|device| device.1)
        .collect::<Vec<_>>();
    for (i, device) in devices.iter().enumerate() {
        println!(
            "found NO.{} device:\nname:{:?}\nphysical_path:{:?}",
            i,
            device.name(),
            device.physical_path(),
        );
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
    id: KeyBoardID,
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
                                    tx.send(KeyEvent::KeyReleased((id, evdev::KeyCode(ev.code()))))?
                                }
                                1_i32 => {
                                    tx.send(KeyEvent::KeyPressed((id, evdev::KeyCode(ev.code()))))?
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
                KeyEvent::KeyPressed((id, key_code)) => {
                    let midi = match my_keyboards.get_midi(&id, &key_code) {
                        Some(midi) => midi,
                        None => {
                            continue;
                        }
                    };
                    play_note(&mut synth, NoteStatus::NoteOn, 0, midi.clone(), 100)?;
                }
                KeyEvent::KeyReleased((id, key_code)) => {
                    let midi = match my_keyboards.get_midi(&id, &key_code) {
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

    // pick the keyboards pairs
    let my_keyboards = KeyBoardPair::init();
    let keyboard_left = pick_device();
    let keyboard_right = pick_device();
    println!("picked the device:{:?}", keyboard_left.name());
    println!("picked the device:{:?}", keyboard_right.name());

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

    // thread to listen to the left_keyboards
    let tx_cloned_a = tx.clone();
    let handle_keyboard_left = thread::spawn(move || {
        listen_keyboards(tx_cloned_a, keyboard_left, KeyBoardID::KeyBoardLeft).unwrap();
    });

    // thread to listen to the right_keyboards
    let tx_cloned_b = tx.clone();
    let handle_keyboard_right = thread::spawn(move || {
        listen_keyboards(tx_cloned_b, keyboard_right, KeyBoardID::KeyBoardRight).unwrap();
    });

    // thread to play the midi
    let handle_playing = thread::spawn(move || {
        playing(rx, &mut synth, &my_keyboards).unwrap();
    });

    // wait for the thread to be done(break inner loop / return)
    handle_keyboard_left.join().unwrap();
    handle_keyboard_right.join().unwrap();
    handle_playing.join().unwrap();
    // // quit the 'fluidsynth'
    // writeln!(synth, "quit")?;
    // synth.flush()?;
    // drop(synth);
    child.wait()?;
    Ok(())
}
