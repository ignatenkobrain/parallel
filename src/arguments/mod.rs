/// Contains all functionality pertaining to parsing, tokenizing, and generating input arguments.
pub mod errors;
mod jobs;
mod man;
mod quote;

use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::process::exit;
use std::time::Duration;

use arrayvec::ArrayVec;
use permutate::Permutator;
use num_cpus;

use super::disk_buffer::{self, DiskBufferTrait, DiskBufferWriter};
use super::input_iterator::InputIterator;
use super::tokenizer::Token;
use self::errors::ParseErr;

// Re-export key items from internal modules.
pub use self::errors::{FileErr, InputIteratorErr};


#[derive(PartialEq)]
enum Mode { Arguments, Command, Inputs, InputsAppend, Files, FilesAppend }

pub const INPUTS_ARE_COMMANDS: u8 = 1;
pub const PIPE_IS_ENABLED:     u8 = 2;
pub const SHELL_ENABLED:       u8 = 4;
pub const QUIET_MODE:          u8 = 8;
pub const VERBOSE_MODE:        u8 = 16;
pub const DASH_EXISTS:         u8 = 32;
pub const DRY_RUN:             u8 = 64;

/// Defines what quoting mode to use when expanding the command.
enum Quoting { None, Basic, Shell }

/// `Args` is a collection of critical options and arguments that were collected at
/// startup of the application.
pub struct Args<'a> {
    pub flags:     u8,
    pub ncores:    usize,
    pub ninputs:   usize,
    pub delay:     Duration,
    pub timeout:   Duration,
    pub arguments: ArrayVec<[Token<'a>; 128]>,
}

impl<'a> Args<'a> {
    pub fn new() -> Args<'a> {
        Args {
            ncores:    num_cpus::get(),
            flags:     0,
            arguments: ArrayVec::new(),
            ninputs:   0,
            delay:     Duration::from_millis(0),
            timeout:   Duration::from_millis(0),
        }
    }

    pub fn parse(&mut self, comm: &mut String, arguments: &[String], unprocessed_path: &Path) -> Result<InputIterator, ParseErr> {
        // Create a write buffer that automatically writes data to the disk when the buffer is full.
        let mut disk_buffer = disk_buffer::DiskBuffer::new(unprocessed_path).write()
            .map_err(|why| ParseErr::File(FileErr::Open(unprocessed_path.to_owned(), why)))?;

        // Temporary stores for input arguments.
        let mut lists: Vec<Vec<String>>     = Vec::new();
        let mut current_inputs: Vec<String> = Vec::with_capacity(1024);
        let mut number_of_arguments = 0;
        let mut max_args = 0;
        let mut quote = Quoting::None;

        if env::args().len() > 1 {
            // The purpose of this is to set the initial parsing mode.
            let (mut mode, mut index) = match arguments[1].as_str() {
                ":::"  => { self.flags |= INPUTS_ARE_COMMANDS; (Mode::Inputs, 2) },
                "::::" => { self.flags |= INPUTS_ARE_COMMANDS; (Mode::Files, 2) },
                _  => (Mode::Arguments, 1)
            };

            if let Mode::Arguments = mode {
                while let Some(argument) = arguments.get(index) {
                    index += 1;
                    let mut char_iter = argument.chars();

                    // If the first character is a '-' then it will be processed as an argument.
                    // We can guarantee that there will always be at least one character.
                    if char_iter.next().unwrap() == '-' {
                        // If the second character exists, everything's OK.
                        let character = char_iter.next().ok_or(ParseErr::InvalidArgument(index-1))?;
                        if character == 'j' {
                            self.ncores = parse_jobs(argument, arguments.get(index), &mut index)?;
                        } else if character == 'n' {
                            max_args = parse_max_args(argument, arguments.get(index), &mut index)?;
                        } else if character != '-' {
                            for character in argument[1..].chars() {
                                match character {
                                    'h' => {
                                        println!("{}", man::MAN_PAGE);
                                        exit(0);
                                    },
                                    'p' => self.flags |= PIPE_IS_ENABLED,
                                    'q' => quote = Quoting::Basic,
                                    's' => self.flags |= QUIET_MODE,
                                    'v' => self.flags |= VERBOSE_MODE,
                                    _ => {
                                        return Err(ParseErr::InvalidArgument(index-1))
                                    }
                                }
                            }
                        } else {
                            // NOTE: Long mode versions of arguments
                            match &argument[2..] {
                                "delay" => {
                                    let val = arguments.get(index).ok_or(ParseErr::DelayNoValue)?;
                                    let seconds = val.parse::<f64>().map_err(|_| ParseErr::DelayNaN(index))?;
                                    self.delay = Duration::from_millis((seconds * 1000f64) as u64);
                                    index += 1;
                                },
                                "dry-run" => self.flags |= DRY_RUN,
                                "help" => {
                                    println!("{}", man::MAN_PAGE);
                                    exit(0);
                                },
                                "jobs" => {
                                    let val = arguments.get(index).ok_or(ParseErr::JobsNoValue)?;
                                    self.ncores = jobs::parse(val)?;
                                    index += 1;
                                },
                                "num-cpu-cores" => {
                                    println!("{}", num_cpus::get());
                                    exit(0);
                                },
                                "max-args" => {
                                    let val = arguments.get(index).ok_or(ParseErr::MaxArgsNoValue)?;
                                    max_args = val.parse::<usize>().map_err(|_| ParseErr::MaxArgsNaN(index))?;
                                    index += 1;
                                }
                                "pipe" => self.flags |= PIPE_IS_ENABLED,
                                "quiet" | "silent" => self.flags |= QUIET_MODE,
                                "quote" => quote = Quoting::Basic,
                                "shellquote" => quote = Quoting::Shell,
                                "timeout" => {
                                    let val = arguments.get(index).ok_or(ParseErr::DelayNoValue)?;
                                    let seconds = val.parse::<f64>().map_err(|_| ParseErr::DelayNaN(index))?;
                                    self.timeout = Duration::from_millis((seconds * 1000f64) as u64);
                                    index += 1;
                                }
                                "verbose" => self.flags |= VERBOSE_MODE,
                                "version" => {
                                    println!("parallel 0.8.0\n\nCrate Dependencies:");
                                    println!("    libc      0.2.18");
                                    println!("    num_cpus  1.2.0");
                                    println!("    permutate 0.2.0");
                                    println!("    arrayvec  0.3.20");
                                    println!("    nodrop    0.1.8");
                                    println!("    odds      0.2.25");
                                    exit(0);
                                }
                                _ => {
                                    return Err(ParseErr::InvalidArgument(index-1));
                                }
                            }
                        }
                    } else {
                        match argument.as_str() {
                            ":::" => {
                                mode = Mode::Inputs;
                                self.flags |= INPUTS_ARE_COMMANDS;
                            },
                            "::::" => {
                                mode = Mode::Files;
                                self.flags |= INPUTS_ARE_COMMANDS;
                            }
                            _ => {
                                // The command has been supplied, and argument parsing is over.
                                comm.push_str(argument);
                                mode = Mode::Command;
                            }
                        }
                        break
                    }
                }
            }

            if let Mode::Command = mode {
                while let Some(argument) = arguments.get(index) {
                    index += 1;
                    match argument.as_str() {
                        // Arguments after `:::` are input values.
                        ":::" | ":::+" => mode = Mode::Inputs,
                        // Arguments after `::::` are files with inputs.
                        "::::" | "::::+" => mode = Mode::Files,
                        // All other arguments are command arguments.
                        _ => {
                            comm.push(' ');
                            comm.push_str(argument);
                            continue
                        }
                    }
                    break
                }
            }

            parse_inputs(arguments, index, &mut current_inputs, &mut lists, &mut mode)?;
            number_of_arguments = write_inputs_to_disk(lists, current_inputs, max_args, &mut disk_buffer)?;
        }

        if disk_buffer.is_empty() {
            number_of_arguments = write_stdin_to_disk(&mut disk_buffer, max_args)?;
        }

        if number_of_arguments == 0 { return Err(ParseErr::NoArguments); }

        // Flush the contents of the buffer to the disk before tokenizing the command argument.
        disk_buffer.flush().map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;

        // Expand the command if quoting is enabled
        match quote {
            Quoting::None  => (),
            Quoting::Basic => *comm = quote::basic(comm.as_str()),
            Quoting::Shell => *comm = quote::shell(comm.as_str()),
        }

        // Return an `InputIterator` of the arguments contained within the unprocessed file.
        let inputs = InputIterator::new(unprocessed_path, number_of_arguments).map_err(ParseErr::File)?;
        Ok(inputs)
    }
}

fn write_stdin_to_disk(disk_buffer: &mut DiskBufferWriter, max_args: usize) -> Result<usize, ParseErr> {
    let mut number_of_arguments = 0;

    let stdin = io::stdin();
    if max_args < 2 {
        for line in stdin.lock().lines() {
            if let Ok(line) = line {
                disk_buffer.write(line.as_bytes()).and_then(|_| disk_buffer.write_byte(b'\n'))
                    .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
                number_of_arguments += 1;
            }
        }
    } else {
        let mut max_args_index = max_args;
        for line in stdin.lock().lines() {
            if let Ok(line) = line {
                if max_args_index == max_args {
                    max_args_index -= 1;
                    number_of_arguments += 1;
                    disk_buffer.write(line.as_bytes())
                        .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
                } else if max_args_index == 1 {
                    max_args_index = max_args;
                    disk_buffer.write_byte(b' ')
                        .and_then(|_| disk_buffer.write(line.as_bytes()))
                        .and_then(|_| disk_buffer.write_byte(b'\n'))
                        .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
                } else {
                    max_args_index -= 1;
                    disk_buffer.write_byte(b' ')
                        .and_then(|_| disk_buffer.write(line.as_bytes()))
                        .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
                }
            }
        }
        if max_args_index != max_args {
            disk_buffer.write_byte(b'\n')
                .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
        }
    }

    Ok(number_of_arguments)
}

fn write_inputs_to_disk(lists: Vec<Vec<String>>, current_inputs: Vec<String>, max_args: usize,
    disk_buffer: &mut DiskBufferWriter) -> Result<usize, ParseErr> {
    let mut number_of_arguments = 0;

    if lists.len() > 1 {
        // Convert the Vec<Vec<String>> into a Vec<Vec<&str>>
        let tmp: Vec<Vec<&str>> = lists.iter()
            .map(|list| list.iter().map(AsRef::as_ref).collect::<Vec<&str>>())
            .collect();

        // Convert the Vec<Vec<&str>> into a Vec<&[&str]>
        let list_array: Vec<&[&str]> = tmp.iter().map(AsRef::as_ref).collect();

        // Create a `Permutator` with the &[&[&str]] as the input.
        let mut permutator = Permutator::new(&list_array[..]);

        // Generate the first permutation's buffer
        let mut permutation_buffer = permutator.next().unwrap();
        {
            let mut iter = permutation_buffer.iter();
            disk_buffer.write(iter.next().unwrap().as_bytes())
                .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
            for element in iter {
                disk_buffer.write_byte(b' ').and_then(|_| disk_buffer.write(element.as_bytes()))
                    .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
            }

            number_of_arguments += 1;
        }

        // Reuse that buffer for each successive permutation
        if max_args < 2 {
            disk_buffer.write_byte(b'\n').map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
            while let Ok(true) = permutator.next_with_buffer(&mut permutation_buffer) {
                let mut iter = permutation_buffer.iter();
                disk_buffer.write(iter.next().unwrap().as_bytes())
                    .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
                for element in iter {
                    disk_buffer.write_byte(b' ').and_then(|_| disk_buffer.write(element.as_bytes()))
                        .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
                }
                disk_buffer.write_byte(b'\n')
                    .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
                number_of_arguments += 1;
            }
        } else {
            let mut max_args_index = max_args - 1;
            while let Ok(true) = permutator.next_with_buffer(&mut permutation_buffer) {
                let mut iter = permutation_buffer.iter();
                if max_args_index == max_args {
                    max_args_index -= 1;
                    number_of_arguments += 1;

                    disk_buffer.write(iter.next().unwrap().as_bytes())
                        .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;

                    for element in iter {
                        disk_buffer.write_byte(b' ').and_then(|_| disk_buffer.write(element.as_bytes()))
                            .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
                    }
                } else if max_args_index == 1 {
                    max_args_index = max_args;
                    disk_buffer.write_byte(b' ')
                        .and_then(|_| disk_buffer.write(iter.next().unwrap().as_bytes()))
                        .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;

                    for element in iter {
                        disk_buffer.write_byte(b' ').and_then(|_| disk_buffer.write(element.as_bytes()))
                            .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
                    }

                    disk_buffer.write_byte(b'\n')
                        .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
                } else {
                    max_args_index -= 1;
                    disk_buffer.write_byte(b' ')
                        .and_then(|_| disk_buffer.write(iter.next().unwrap().as_bytes()))
                        .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;

                    for element in iter {
                        disk_buffer.write_byte(b' ').and_then(|_| disk_buffer.write(element.as_bytes()))
                            .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
                    }
                }
            }
        }
    } else if max_args < 2 {
        for input in current_inputs {
            disk_buffer.write(input.as_bytes())
                .and_then(|_| disk_buffer.write_byte(b'\n'))
                .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
            number_of_arguments += 1;
        }
    } else {
        for chunk in current_inputs.chunks(max_args) {
            let max_index = chunk.len()-1;
            let mut index = 0;
            number_of_arguments += 1;

            while index != max_index {
                disk_buffer.write(chunk[index].as_bytes())
                    .and_then(|_| disk_buffer.write_byte(b' '))
                    .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
                index += 1;
            }
            disk_buffer.write(chunk[max_index].as_bytes())
                .and_then(|_| disk_buffer.write_byte(b'\n'))
                .map_err(|why| FileErr::Write(disk_buffer.path.clone(), why))?;
        }
    }
    Ok(number_of_arguments)
}

fn parse_inputs(arguments: &[String], mut index: usize, current_inputs: &mut Vec<String>, lists: &mut Vec<Vec<String>>,
    mode: &mut Mode) -> Result<(), ParseErr> {
    let mut append_list = &mut Vec::new();

    macro_rules! switch_mode {
        ($mode:expr) => {{
            match *mode {
                Mode::InputsAppend | Mode::FilesAppend => merge_lists(current_inputs, append_list),
                _ => (),
            }
            *mode = $mode;
            if !current_inputs.is_empty() {
                lists.push(current_inputs.clone());
                current_inputs.clear();
            }
        }};
        (append $mode:expr) => {{
            match *mode {
                Mode::InputsAppend | Mode::FilesAppend => merge_lists(current_inputs, append_list),
                _ => (),
            }
            *mode = $mode;
        }};
    }

    // Parse each and every input argument supplied to the program.
    while let Some(argument) = arguments.get(index) {
        index += 1;
        match argument.as_str() {
            // `:::` denotes that the next set of inputs will be added to a new list.
            ":::"  => switch_mode!(Mode::Inputs),
            // `:::+` denotes that the next set of inputs will be added to the current list.
            ":::+" => switch_mode!(append Mode::InputsAppend),
            // `::::` denotes that the next set of inputs will be added to a new list.
            "::::"  => switch_mode!(Mode::Files),
            // `:::+` denotes that the next set of inputs will be added to the current list.
            "::::+" => switch_mode!(append Mode::FilesAppend),
            // All other arguments will be added to the current list.
            _ => match *mode {
                Mode::Inputs       => current_inputs.push(argument.clone()),
                Mode::InputsAppend => append_list.push(argument.clone()),
                Mode::Files        => file_parse(current_inputs, argument)?,
                Mode::FilesAppend  => file_parse(append_list, argument)?,
                _                  => unreachable!()
            }
        }
    }

    if !append_list.is_empty() {
        match *mode {
            Mode::InputsAppend | Mode::FilesAppend => merge_lists(current_inputs, append_list),
            _ => (),
        }
    }

    if !current_inputs.is_empty() {
        lists.push(current_inputs.clone());
    }

    Ok(())
}

/// Parses the `max_args` value, `-n3` or `-n 3`, and optionally increments the index if necessary.
fn parse_max_args(argument: &str, next_argument: Option<&String>,index: &mut usize) -> Result<usize, ParseErr> {
    if argument.len() > 2 {
        Ok(argument[2..].parse::<usize>().map_err(|_| ParseErr::MaxArgsNaN(*index))?)
    } else {
        *index += 1;
        let argument = next_argument.ok_or(ParseErr::MaxArgsNoValue)?;
        Ok(argument.parse::<usize>().map_err(|_| ParseErr::MaxArgsNaN(*index))?)
    }
}

/// Merges an `append` list to the `original` list, draining the `append` list in the process.
/// Excess arguments will be truncated, and therefore lost.
fn merge_lists(original: &mut Vec<String>, append: &mut Vec<String>) {
    if original.len() > append.len() {
        original.truncate(append.len());
    }
    for (input, element) in original.iter_mut().zip(append.drain(..)) {
        input.push(' ');
        input.push_str(&element);
    }
}

/// Parses the jobs value, and optionally increments the index if necessary.
fn parse_jobs(argument: &str, next_argument: Option<&String>, index: &mut usize) -> Result<usize, ParseErr> {
    let ncores = if argument.len() > 2 {
        jobs::parse(&argument[2..])?
    } else {
        *index += 1;
        jobs::parse(next_argument.ok_or(ParseErr::JobsNoValue)?)?
    };

    Ok(ncores)
}

/// Attempts to open an input argument and adds each line to the `inputs` list.
fn file_parse<P: AsRef<Path>>(inputs: &mut Vec<String>, path: P) -> Result<(), ParseErr> {
    let path = path.as_ref();
    let file = fs::File::open(path).map_err(|err| ParseErr::File(FileErr::Open(path.to_owned(), err)))?;
    for line in BufReader::new(file).lines() {
        if let Ok(line) = line { inputs.push(line); }
    }
    Ok(())
}
