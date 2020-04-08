use crate::less::formats::Message;
use crate::less::reader::PagedReader;
use crate::less::screen_move_handler::ScreenMoveHandler;
use crossbeam_channel::Sender;
use memmap::{Mmap, MmapMut};
use signal_hook::{iterator::Signals, SIGINT, SIGWINCH};
use std::fs::{File, OpenOptions};
use std::io::{stdin, stdout, Read, Stdout, Write};
use std::path::PathBuf;
use std::{fs, thread};
use termion::event::Key;
use termion::input::TermRead;
use termion::raw::{IntoRawMode, RawTerminal};
use termion::screen::AlternateScreen;
use termion::{is_tty, terminal_size};

mod formats;
mod reader;
mod screen_move_handler;

fn read_from_pipe(screen: &mut RawTerminal<AlternateScreen<Stdout>>) -> Mmap {
    let (_cols, mut rows) = terminal_size().unwrap_or_else(|_| (80, 80));

    let (sender, receiver) = crossbeam_channel::unbounded();
    let tempdir = tempdir::TempDir::new("lesser").expect("Tempdir");
    let path: PathBuf = tempdir.path().join("map_mut");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&path)
        .expect("Create file");

    spawn_stdin_handler(sender);
    for str_buf in receiver {
        file.write(str_buf.as_bytes()).expect("Write file");
    }
    file.flush().expect("flush");
    let mut mmap = unsafe { MmapMut::map_mut(&file).expect("Mmmap") };
    mmap.make_read_only().expect("Readonly")
}

pub fn run(filename: Option<PathBuf>) -> std::io::Result<()> {
    let screen = AlternateScreen::from(stdout()).into_raw_mode().unwrap();
    let mut screen = termion::cursor::HideCursor::from(screen);

    let (sender, receiver) = crossbeam_channel::bounded(100);
    //TODO: ioctl invalid if run inside intellij's run.
    let mmap = if let Some(filename) = filename {
        let file_size = std::fs::metadata(&filename)?.len();
        if file_size > 0 {
            let file = File::open(filename)?;
            unsafe { Mmap::map(&file).expect("failed to map the file") }
        } else {
            MmapMut::map_anon(1).expect("Anon mmap").make_read_only()?
        }
    } else {
        if !is_tty(&stdin()) {
            read_from_pipe(&mut screen)
        } else {
            unimplemented!();
            MmapMut::map_anon(1).expect("Anon mmap").make_read_only()?
            // TODO: Error, must specify an input!
        }
    };

    let paged_reader = PagedReader::new(mmap);
    let mut screen_move_handler: ScreenMoveHandler = ScreenMoveHandler::new(paged_reader);
    spawn_key_pressed_handler(sender.clone());
    spawn_signal_handler(sender.clone());
    let (cols, rows) = terminal_size().unwrap_or_else(|_| (80, 80));

    let initial_screen = screen_move_handler.initial_screen(rows, cols)?;
    write_screen(&mut screen, initial_screen)?;

    'main_loop: for message in receiver {
        let (cols, rows) = terminal_size().unwrap_or_else(|_| (80, 80));
        let page = match message {
            Message::ScrollUpPage => screen_move_handler.move_up(rows, cols)?,
            Message::ScrollLeftPage => screen_move_handler.move_left(rows, cols)?,
            Message::ScrollRightPage => screen_move_handler.move_right(rows, cols)?,
            Message::ScrollDownPage => screen_move_handler.move_down(rows, cols)?,
            Message::Reload => screen_move_handler.reload(rows, cols)?,
            Message::Exit => break 'main_loop,
        };
        write_screen(&mut screen, page)?;
    }

    Ok(())
}

fn spawn_signal_handler(sender: Sender<Message>) {
    let signals = Signals::new(&[SIGWINCH, SIGINT]).expect("Signal handler");

    thread::spawn(move || {
        for sig in signals.forever() {
            let msg = match sig {
                signal_hook::SIGWINCH => Message::Reload,
                _ => Message::Exit,
            };
            sender.send(msg).unwrap();
            debug!("Received signal {:?}", sig);
        }
    });
}

fn spawn_stdin_handler(sender: Sender<String>) {
    let mut stdin = stdin();
    loop {
        let mut buffer = String::new();
        match stdin.read_to_string(&mut buffer) {
            Ok(read_len) => {
                if read_len == 0 {
                    return;
                }
                sender.send(buffer).unwrap();
            }
            Err(error) => {
                eprintln!("Error: {:?}", error);
                break;
            }
        }
    }
}

fn spawn_key_pressed_handler(sender: Sender<Message>) {
    thread::spawn(move || {
        let tty = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .expect("Open tty");

        // Can use the tty_input for keys while also reading stdin for data.
        let tty_input = tty
            .try_clone()
            .expect("Try clone")
            .into_raw_mode()
            .expect("Into raw mode");

        for c in tty_input.try_clone().unwrap().keys() {
            let message = match c.expect("read keys") {
                Key::Char('q') => Message::Exit,
                Key::Ctrl(c) if c.to_string().as_str() == "c" => Message::Exit,
                Key::Left => Message::ScrollLeftPage,
                Key::Right => Message::ScrollRightPage,
                Key::Up => Message::ScrollUpPage,
                // Goes down by default.
                _ => Message::ScrollDownPage,
            };
            sender.send(message).unwrap();
        }
    });
}

/// If page is None, then we made a read which didn't return anything.
fn write_screen(
    screen: &mut RawTerminal<AlternateScreen<Stdout>>,
    page: Option<String>,
) -> std::io::Result<()> {
    if let Some(page) = page {
        write!(screen, "{}", termion::clear::All)?;
        write!(screen, "{}", termion::cursor::Goto(1, 1))?;
        write!(screen, "{}", page)?;
    }
    screen.flush().expect("Failed to flush");
    Ok(())
}

fn write_line(
    screen: &mut RawTerminal<AlternateScreen<Stdout>>,
    line: Option<String>,
) -> std::io::Result<()> {
    if let Some(line) = line {
        write!(screen, "{}", line)?;
    }
    screen.flush().expect("Failed to flush");
    Ok(())
}
