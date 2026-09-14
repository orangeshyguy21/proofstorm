//! Deliberately small child processes for the Linux supervisor contract.
use std::{
    io::{Read, Write},
    process::Command,
    time::Duration,
};

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args[0].as_str() {
        "emit" => {
            print!("{}", args[1]);
            if let Some(padding) = args.get(5) {
                std::io::stdout()
                    .write_all(&vec![b' '; padding.parse().unwrap()])
                    .unwrap();
            }
            eprint!("{}", args[2]);
            std::io::stdout().flush().unwrap();
            std::io::stderr().flush().unwrap();
            std::thread::sleep(Duration::from_millis(args[4].parse().unwrap()));
            std::process::exit(args[3].parse().unwrap());
        }
        "repeat" => {
            std::io::stdout()
                .write_all(args[1].repeat(args[2].parse().unwrap()).as_bytes())
                .unwrap();
            std::io::stderr()
                .write_all(args[3].repeat(args[4].parse().unwrap()).as_bytes())
                .unwrap();
        }
        "binary" => {
            let bytes = vec![0; args[1].parse().unwrap()];
            std::io::stdout().write_all(&bytes).unwrap();
            std::io::stderr().write_all(&bytes).unwrap();
        }
        "stdin" => {
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            assert_eq!(input, args[1].repeat(args[2].parse().unwrap()).as_bytes());
        }
        "argv" => assert_eq!(args[1], args[2]),
        #[cfg(target_os = "linux")]
        "escape" => {
            // The supervisor must reap descendants even after they leave its session.
            nix::unistd::setsid().unwrap();
            std::fs::write(&args[1], std::process::id().to_string()).unwrap();
            std::thread::sleep(Duration::from_secs(120));
        }
        "parent" => {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["escape", &args[1]])
                .spawn()
                .unwrap();
            let _ = child.wait();
        }
        _ => panic!("invalid fixture command"),
    }
}
