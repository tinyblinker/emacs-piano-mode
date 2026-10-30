use std::io::Write;

use nix::{
    poll::PollTimeout,
    sys::epoll::{Epoll, EpollCreateFlags, EpollEvent, EpollFlags},
};

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

fn main() -> std::io::Result<()> {
    // pick the keyboazrd device
    let device = pick_device();
    println!("picked the device:{}", device);

    // read event
    print_event(device)?;
    Ok(())
}
