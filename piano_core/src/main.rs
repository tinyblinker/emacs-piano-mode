use clap::Parser;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Error, ErrorKind, Host, HostId, OutputCallbackInfo, SampleFormat, SupportedStreamConfig,
};
use evdev::KeyCode;
use nix::{
    poll::PollTimeout,
    sys::epoll::{Epoll, EpollCreateFlags, EpollEvent, EpollFlags},
};
use ringbuf::traits::{Consumer, Producer, Split};
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufWriter, Write},
    process::{ChildStdin, Command, Stdio},
    sync::{Arc, RwLock},
    thread::{self},
    time::Duration,
};

#[derive(Parser, Debug)]
#[command(version, about = "CPAL feedback example", long_about = None)]
struct Opt {
    /// The input audio device to use
    #[arg(short, long, value_name = "IN")]
    input_device: Option<String>,

    /// The output audio device to use
    #[arg(short, long, value_name = "OUT")]
    output_device: Option<String>,

    /// Specify the delay between input and output
    #[arg(short, long, value_name = "DELAY_MS", default_value_t = 150.0)]
    latency: f32,

    /// Use the PipeWire host. Requires `--features pipewire`.
    #[arg(long, default_value_t = false)]
    pipewire: bool,
}

const STREAM_CHANNELS: usize = 2;
const STREAM_SAMPLERATE: usize = 44100;
const STREAM_LATENCY: usize = 150; // latency (ms)

#[derive(Debug, Clone)]
struct KeyBoard {
    // we need id to classified two different keyboards
    id: usize,
    // a hashmap that store all the status of the keyboard
    keys_status_set: HashMap<evdev::KeyCode, KeyStatus>,
}

impl KeyBoard {
    pub fn init(id: usize) -> Self {
        Self {
            id: id,
            keys_status_set: HashMap::<evdev::KeyCode, KeyStatus>::new(),
        }
    }
    pub fn update_keyboard(&mut self, key: evdev::KeyCode, status: KeyStatus) {
        self.keys_status_set.insert(key, status);
    }
}

impl TryFrom<i32> for KeyStatus {
    type Error = anyhow::Error;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0_i32 => Ok(KeyStatus::KeyUp),
            1_i32 => Ok(KeyStatus::KeyDown),
            2_i32 => Ok(KeyStatus::KeyPressed),
            _ => Err(anyhow::Error::msg(
                "The pressed key has an unknowned KeyStatus",
            )),
        }
    }
}

#[derive(Clone, Debug, Copy, PartialEq, Eq)]
enum KeyStatus {
    KeyUp,
    KeyDown,
    KeyPressed,
}

#[derive(Debug, Clone)]
struct PcmData {
    left: Vec<f32>,
    right: Vec<f32>,
}

// impl PcmData -> let PCM data generate form "left" & "right"
impl PcmData {
    pub fn new() -> Self {
        Self {
            left: vec![0_f32; STREAM_SAMPLERATE * 3],
            right: vec![0_f32; STREAM_SAMPLERATE * 3],
        }
    }
    pub fn generate_pcm(&self) -> Vec<f32> {
        assert_eq!(
            self.left.len(),
            self.right.len(),
            "left and right's pcm data len must be equal"
        );
        let mut output: Vec<f32> = Vec::with_capacity(self.left.len() * 2);
        for (&l, &r) in self.left.iter().zip(self.right.iter()) {
            output.push(l);
            output.push(r);
        }
        output
    }
}

fn pick_device() -> evdev::Device {
    println!("pick device!");
    let mut args = std::env::args_os(); // get the command line args
    args.next(); // skip the first arg in command line
    if let Some(a_device) = args.next() {
        evdev::Device::open(a_device).unwrap()
    } else {
        // no device is specified, so we enumerate all and
        // let the user to choose one

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
}

fn sync_keyboards(mut device: evdev::Device, keyboard: Arc<RwLock<KeyBoard>>) {
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
                    if ev.event_type() == evdev::EventType::KEY {
                        // println!("ev.value()={:?}", ev.value());
                        // println!("ev.code()={:?}", ev.code());
                        match keyboard.try_write() {
                            Ok(mut guard) => {
                                guard.update_keyboard(
                                    evdev::KeyCode(ev.code()),
                                    ev.value().try_into().unwrap(),
                                );
                            }
                            Err(_) => {
                                println!(
                                    "####[debug]try_write() \"keyboard\" failed in handle render"
                                );
                            }
                        }
                        // just maintain the "keyboard" struct to share with the 'rustysynth' for
                        // rendering the PCM data
                        println!("####[debug]keyboard={:?}", keyboard);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // wait forever for bytes available on dev
                epoll.wait(&mut events, PollTimeout::NONE).unwrap();
            }
            Err(e) => {
                println!("err:{e}");
                break;
            }
        }
    }
}

fn generate_a_note_to_pcm(
    channel: i32,
    key: i32,
    velocity: i32,
    synthesizer: &mut Synthesizer,
) -> PcmData {
    // // 'rustysynth' crate => PCM data
    // let mut sf2 = File::open("TimGM6mb.sf2").unwrap();
    // let sound_font = Arc::new(SoundFont::new(&mut sf2).unwrap());

    // // create the synthesizer
    // let settings = SynthesizerSettings::new(STREAM_SAMPLERATE as i32);
    // let mut synthesizer = Synthesizer::new(&sound_font, &settings).unwrap();

    // play some notes
    synthesizer.note_on(channel, key, velocity);

    // the output buffer(3 seconds)
    let mut pcm: PcmData = PcmData::new();

    // Render the waveform
    synthesizer.render(&mut pcm.left, &mut pcm.right);

    // left, right => now have the data inside(PCM)
    pcm
}

type ProducerType = ringbuf::wrap::caching::Caching<
    Arc<ringbuf::SharedRb<ringbuf::storage::Heap<f32>>>,
    true,
    false,
>;
type ConsumerType = ringbuf::wrap::caching::Caching<
    Arc<ringbuf::SharedRb<ringbuf::storage::Heap<f32>>>,
    false,
    true,
>;

#[derive(Debug)]
enum NoteStatus {
    NoteOn,
    NoteOff,
}

fn plat_note(
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

fn main() -> anyhow::Result<(), anyhow::Error> {
    // init extra bin 'fluidsynth'(https://github.com/fluidsynth/fluidsynth)
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

    plat_note(&mut synth, NoteStatus::NoteOn, 0, 60, 100)?;

    loop {}
    writeln!(synth, "quit")?;
    synth.flush()?;
    drop(synth);
    child.wait()?;
    Ok(())

    // // init a keyboard(virtual) Arc<Rwlock<_>>
    // let keyboard_a = Arc::new(RwLock::new(KeyBoard::init(0)));

    // // pick the keyboard device
    // let device = pick_device();
    // println!("picked the device:{}", device);

    // // define a ringbuf for 'PCM' data (from 'handle_keyboards' to 'cpal_run')
    // // The buffer to share samples
    // let latency_frames = (STREAM_LATENCY as f32 / 1_000.0) * STREAM_SAMPLERATE as f32;
    // let latency_samples = latency_frames as usize * STREAM_CHANNELS as usize;
    // let pcm_ring = ringbuf::HeapRb::<f32>::new(latency_samples * 2);
    // let (mut producer, consumer): (ProducerType, ConsumerType) = pcm_ring.split();

    // // read event thread (read the event and send it to the
    // // 'rustysynth' to render as PCM data and 'cpal' to
    // // play the PCM data)
    // let input_keyboard = Arc::clone(&keyboard_a);
    // let handle_keyboards = thread::spawn(move || {
    //     sync_keyboards(device, input_keyboard);
    // });

    // // 'rustysynth' crate => PCM data
    // let mut sf2 = File::open("TimGM6mb.sf2").unwrap();
    // let sound_font = Arc::new(SoundFont::new(&mut sf2).unwrap());
    // // create the synthesizer
    // let settings = SynthesizerSettings::new(STREAM_SAMPLERATE as i32);
    // let mut synthesizer = Synthesizer::new(&sound_font, &settings).unwrap();
    // let output_keyboard = Arc::clone(&keyboard_a);
    // let handle_render = thread::spawn(move || {
    //     loop {
    //         // read the hashmap of keyboard,base on it to play notes
    //         let mut keyboard_hashmap: HashMap<KeyCode, KeyStatus> = HashMap::new();
    //         match output_keyboard.try_read() {
    //             Ok(guard) => {
    //                 let tmp = guard.keys_status_set.clone();
    //                 keyboard_hashmap = tmp;
    //             }
    //             Err(_) => {
    //                 println!("####[debug]try_read() \"keyboard\" failed in handle render");
    //             }
    //         }

    //         // for each key, base on its status to play some notes?
    //         for (key, value) in keyboard_hashmap.iter() {
    //             match *key {
    //                 KeyCode::KEY_1 if *value == KeyStatus::KeyDown => {
    //                     let pcm: PcmData = generate_a_note_to_pcm(0, 40, 100, &mut synthesizer);
    //                     let pcm_raw = pcm.generate_pcm();
    //                     producer.push_slice(&pcm_raw);
    //                 }
    //                 KeyCode::KEY_2 if *value == KeyStatus::KeyDown => {
    //                     let pcm: PcmData = generate_a_note_to_pcm(0, 50, 100, &mut synthesizer);
    //                     let pcm_raw = pcm.generate_pcm();
    //                     producer.push_slice(&pcm_raw);
    //                 }
    //                 KeyCode::KEY_3 if *value == KeyStatus::KeyDown => {
    //                     let pcm: PcmData = generate_a_note_to_pcm(0, 60, 100, &mut synthesizer);
    //                     let pcm_raw = pcm.generate_pcm();
    //                     producer.push_slice(&pcm_raw);
    //                 }
    //                 KeyCode::KEY_4 if *value == KeyStatus::KeyDown => {
    //                     let pcm: PcmData = generate_a_note_to_pcm(0, 70, 100, &mut synthesizer);
    //                     let pcm_raw = pcm.generate_pcm();
    //                     producer.push_slice(&pcm_raw);
    //                 }
    //                 _ => {}
    //             }
    //         }
    //     }
    // });

    // // CPAL crate: PCM->linux alsa(play the PCM)
    // let opt = Opt::parse();
    // let host = cpal_init_host(&opt);
    // let handle_cpal = thread::spawn(move || {
    //     cpal_run(&opt, &host, consumer);
    // });

    // // wait for the thread to be done(break inner loop / return)
    // handle_keyboards.join().unwrap();
    // handle_render.join().unwrap();
    // handle_cpal.join().unwrap();

    // Ok(())
}

fn cpal_init_host(opt: &Opt) -> cpal::Host {
    #[allow(unused_assignments)]
    let mut pipewire_host_id: Result<HostId, Error> = Err(ErrorKind::HostUnavailable.into());

    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd"
    ))]
    {
        #[cfg(feature = "pipewire")]
        {
            pipewire_host_id = Ok(HostId::PipeWire);
        }
    }

    // Manually check for flags. Can be passed through cargo with -- e.g.
    // cargo run --release --example beep --features jack -- --jack
    if opt.pipewire {
        pipewire_host_id
            .and_then(cpal::host_from_id)
            .expect("make sure `--features pipewire` is specified, and the platform is supported")
    } else {
        cpal::default_host()
    }
}

fn cpal_run(opt: &Opt, host: &Host, consumer: ConsumerType) {
    let output_device = if let Some(ref device) = opt.output_device {
        let id = &device.parse().expect("failed to parse output device id");
        host.device_by_id(id)
    } else {
        host.default_output_device()
    }
    .expect("failed to find output device");

    println!("Using output device: \"{}\"", output_device.id().unwrap());

    // We'll try and use the same configuration between streams to keep it simple.
    // let input_config = input_device.default_input_config().unwrap();
    let output_config: SupportedStreamConfig = output_device.default_output_config().unwrap();
    println!("####[debug]output_config = {:?}", output_config);

    match output_config.sample_format() {
        SampleFormat::F32 => {
            // edit stream configs
            let mut config: cpal::StreamConfig = output_config.into();
            config.channels = STREAM_CHANNELS as u16;
            config.sample_rate = STREAM_SAMPLERATE as u32;
            println!("####[debug]config = {:?}", config);

            // build and play the stream
            run::<f32>(&output_device, config, opt.latency, consumer);
        }
        sample_format => panic!("Unsupported sample format '{sample_format}'"),
    }
}

fn run<T>(
    output_device: &cpal::Device,
    config: cpal::StreamConfig,
    latency_ms: f32,
    mut consumer: ConsumerType,
) where
    T: cpal::SizedSample + std::fmt::Debug + Send + 'static + cpal::FromSample<f32>,
{
    // // Create a delay in case the input and output devices aren't synced.
    // let latency_frames = (latency_ms / 1_000.0) * config.sample_rate as f32;
    // let latency_samples = latency_frames as usize * config.channels as usize;

    // // The buffer to share samples
    // let ring = ringbuf::HeapRb::<T>::new(latency_samples * 2);
    // let (mut producer, mut consumer) = ring.split();

    // Pre-fill with silence equal to the length of the delay.
    // for _ in 0..latency_samples {
    //     // The ring buffer has twice as much space as necessary to add latency here,
    //     // so this should never fail
    //     producer.try_push(T::EQUILIBRIUM).unwrap();
    // }

    // // push 3s' data to the ringbuf
    // let pcm_push_data: Vec<T> = pcm_data
    //     .generate_pcm()
    //     .iter()
    //     .map(|sample| T::from_sample_(sample.clone()))
    //     .collect();

    let output_data_fn = move |data: &mut [T], _: &OutputCallbackInfo| {
        let tmp = &mut [0_f32; STREAM_SAMPLERATE * STREAM_CHANNELS];
        let read = consumer.pop_slice(tmp);
        data.iter_mut()
            .zip(tmp.iter())
            .for_each(|(data_dst, data_tmp)| {
                *data_dst = T::from_sample(*data_tmp);
            });
        if read < data.len() {
            data[read..].fill(T::EQUILIBRIUM);
            eprintln!("input stream fell behind: try increasing latency");
        }
    };

    // Build streams.
    println!(
        "Attempting to build both streams with {} samples and `{config:?}`.",
        T::FORMAT
    );
    // let input_stream = input_device.build_input_stream(config, input_data_fn, err_fn, None)?;
    let output_stream = output_device
        .build_output_stream(config, output_data_fn, err_fn, None)
        .unwrap();
    println!("Successfully built streams.");

    // Play the streams.
    println!("Starting the input and output streams with `{latency_ms}` milliseconds of latency.");
    // input_stream.play()?;
    output_stream.play().unwrap();

    // let mut sent: usize = 0;
    // loop {
    //     if sent < pcm_push_data.len() {
    //         let i = producer.push_slice(&pcm_push_data[sent..]);
    //         sent += i;
    //         sleep(Duration::from_millis(70));
    //     } else {
    //         break;
    //     }
    // }

    // Run for 10 seconds before closing.
    println!("Playing for 10 seconds...");
    std::thread::sleep(std::time::Duration::from_secs(10));
    // drop(input_stream);
    drop(output_stream);
    println!("Done!");
}

fn err_fn(err: Error) {
    match err.kind() {
        ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied => {
            eprintln!("{err}")
        }
        _ => eprintln!("Stream error: {err}"),
    }
}
