use clap::Parser;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Error, ErrorKind, Host, HostId, InputCallbackInfo, OutputCallbackInfo, SampleFormat,
};
use nix::{
    poll::PollTimeout,
    sys::epoll::{Epoll, EpollCreateFlags, EpollEvent, EpollFlags},
};
use ringbuf::traits::{Consumer, Producer, Split};
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use std::{fs::File, io::Write, sync::Arc, thread, time::Duration};

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

fn print_event(mut device: evdev::Device) -> std::io::Result<()> {
    // set the device(like a kind of fd) as nonblocking
    let _ = device.set_nonblocking(true)?;

    // create epoll handle and attach raw_fd
    let epoll = Epoll::new(EpollCreateFlags::EPOLL_CLOEXEC)?;
    let event = EpollEvent::new(EpollFlags::EPOLLIN, 0);
    epoll.add(&device, event)?;

    // set the event to wait
    let mut events = [EpollEvent::empty(); 2];
    println!("Event:");
    loop {
        match device.fetch_events() {
            Ok(ev_iter) => {
                for ev in ev_iter {
                    println!("{ev:?}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // wait forever for bytes available on dev
                epoll.wait(&mut events, PollTimeout::NONE)?;
            }
            Err(e) => {
                println!("err:{e}");
                break;
            }
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<(), anyhow::Error> {
    // // pick the keyboard device
    // let device = pick_device();
    // println!("picked the device:{}", device);

    // // read event
    // print_event(device)?;

    // 'rustysynth' crate => PCM data
    // play sounds
    let mut sf2 = File::open("TimGM6mb.sf2").unwrap();
    let sound_font = Arc::new(SoundFont::new(&mut sf2).unwrap());

    // create the synthesizer
    let settings = SynthesizerSettings::new(44100);
    let mut synthesizer = Synthesizer::new(&sound_font, &settings).unwrap();

    // play some notes
    synthesizer.note_on(0, 60, 100);
    synthesizer.note_on(0, 64, 100);
    synthesizer.note_on(0, 67, 100);

    // the output buffer(3 seconds)
    let sample_count = (3 * settings.sample_rate) as usize;
    let mut left: Vec<f32> = vec![0_f32; sample_count];
    let mut right: Vec<f32> = vec![0_f32; sample_count];

    // Render the waveform
    synthesizer.render(&mut left[..], &mut right[..]);

    // CPAL crate: PCM->linux alsa(play the PCM)
    let opt = Opt::parse();
    let host = cpal_init_host(&opt);
    cpal_run(&opt, &host)?;
    Ok(())
}

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

fn cpal_init_host(opt: &Opt) -> cpal::Host {
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

fn cpal_run(opt: &Opt, host: &Host) -> anyhow::Result<()> {
    // Find devices.()
    let input_device = if let Some(ref device) = opt.input_device {
        let id = &device.parse().expect("failed to parse input device id");
        host.device_by_id(id)
    } else {
        host.default_input_device()
    }
    .expect("failed to find input device");

    let output_device = if let Some(ref device) = opt.output_device {
        let id = &device.parse().expect("failed to parse output device id");
        host.device_by_id(id)
    } else {
        host.default_output_device()
    }
    .expect("failed to find output device");

    println!("Using input device: \"{}\"", input_device.id().unwrap());
    println!("Using output device: \"{}\"", output_device.id().unwrap());

    // We'll try and use the same configuration between streams to keep it simple.
    let input_config = input_device.default_input_config().unwrap();
    let output_config = output_device.default_output_config().unwrap();
    assert_eq!(
        input_config.sample_format(),
        output_config.sample_format(),
        "input and output devices must share a sample format for this example"
    );
    match input_config.sample_format() {
        SampleFormat::I8 => run::<i8>(
            &input_device,
            &output_device,
            input_config.into(),
            opt.latency,
        ),
        SampleFormat::I16 => run::<i16>(
            &input_device,
            &output_device,
            input_config.into(),
            opt.latency,
        ),
        SampleFormat::I32 => run::<i32>(
            &input_device,
            &output_device,
            input_config.into(),
            opt.latency,
        ),
        SampleFormat::I64 => run::<i64>(
            &input_device,
            &output_device,
            input_config.into(),
            opt.latency,
        ),
        SampleFormat::U8 => run::<u8>(
            &input_device,
            &output_device,
            input_config.into(),
            opt.latency,
        ),
        SampleFormat::U16 => run::<u16>(
            &input_device,
            &output_device,
            input_config.into(),
            opt.latency,
        ),
        SampleFormat::U32 => run::<u32>(
            &input_device,
            &output_device,
            input_config.into(),
            opt.latency,
        ),
        SampleFormat::U64 => run::<u64>(
            &input_device,
            &output_device,
            input_config.into(),
            opt.latency,
        ),
        SampleFormat::F32 => run::<f32>(
            &input_device,
            &output_device,
            input_config.into(),
            opt.latency,
        ),
        SampleFormat::F64 => run::<f64>(
            &input_device,
            &output_device,
            input_config.into(),
            opt.latency,
        ),
        sample_format => panic!("Unsupported sample format '{sample_format}'"),
    }
}

fn run<T>(
    input_device: &cpal::Device,
    output_device: &cpal::Device,
    config: cpal::StreamConfig,
    latency_ms: f32,
) -> anyhow::Result<()>
where
    T: cpal::SizedSample + std::fmt::Debug + Send + 'static,
{
    // Create a delay in case the input and output devices aren't synced.
    let latency_frames = (latency_ms / 1_000.0) * config.sample_rate as f32;
    let latency_samples = latency_frames as usize * config.channels as usize;

    // The buffer to share samples
    let ring = ringbuf::HeapRb::<T>::new(latency_samples * 2);
    let (mut producer, mut consumer) = ring.split();

    // Pre-fill with silence equal to the length of the delay.
    for _ in 0..latency_samples {
        // The ring buffer has twice as much space as necessary to add latency here,
        // so this should never fail
        producer.try_push(T::EQUILIBRIUM).unwrap();
    }

    let input_data_fn = move |data: &[T], _: &InputCallbackInfo| {
        if producer.push_slice(data) < data.len() {
            eprintln!("output stream fell behind: try increasing latency");
        }
    };

    let output_data_fn = move |data: &mut [T], _: &OutputCallbackInfo| {
        let read = consumer.pop_slice(data);
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
    let input_stream = input_device.build_input_stream(config, input_data_fn, err_fn, None)?;
    let output_stream = output_device.build_output_stream(config, output_data_fn, err_fn, None)?;
    println!("Successfully built streams.");

    // Play the streams.
    println!("Starting the input and output streams with `{latency_ms}` milliseconds of latency.");
    input_stream.play()?;
    output_stream.play()?;

    // Run for 10 seconds before closing.
    println!("Playing for 10 seconds...");
    std::thread::sleep(std::time::Duration::from_secs(10));
    drop(input_stream);
    drop(output_stream);
    println!("Done!");
    Ok(())
}

/// output callback function helper
/// PCM --> output
fn write_data<T>(output: &mut [T], channels: usize, next_sample: &mut dyn FnMut() -> f32)
where
    T: cpal::Sample + cpal::FromSample<f32>,
{
    for frame in output.chunks_mut(channels) {
        let value: T = T::from_sample(next_sample());
        for sample in frame.iter_mut() {
            *sample = value;
        }
    }
}

fn err_fn(err: Error) {
    match err.kind() {
        ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied => {
            eprintln!("{err}")
        }
        _ => eprintln!("Stream error: {err}"),
    }
}
