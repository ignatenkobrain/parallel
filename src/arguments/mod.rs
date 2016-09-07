use std::env;
use std::io::{self, BufRead, BufReader};
use std::process::exit;
use tokenizer::{Token, tokenize};
use permutate::Permutator;
use num_cpus;

mod jobs;
mod man;

use std::fs;

/// `Args` is a collection of critical options and arguments that were collected at
/// startup of the application.
pub struct Args {
    /// The number of jobs to run in parallel.
    pub ncores:     usize,
    /// Whether stdout/stderr of each job should be handled serially or not.
    ///
    /// **NOTE:** This has a performance cost when enabled.
    pub grouped:    bool,
    /// Whether the platform's shell should be used or not.
    ///
    /// **NOTE:** This has a large performance cost.
    pub uses_shell: bool,
    /// Evaluates to true if the first argument is either `:::` or `::::`.
    pub inputs_are_commands: bool,
    /// If set to true, the application will print information about running tasks.
    pub verbose:    bool,
    /// The command arguments collected as a list of `Token`s
    pub arguments:  Vec<Token>,
    /// The inputs supplied that will be used with `arguments`.
    pub inputs:     Vec<String>
}

/// The error type for the argument module.
pub enum ParseErr {
    /// An error occured opening an input file.
    InputFileError(String, String),
    /// The value supplied for `--jobs` is not a number.
    JobsNaN(String),
    /// No value was supplied for '--jobs'
    JobsNoValue,
    /// The argument supplied is not a valid argument.
    InvalidArgument(String),
}

#[derive(PartialEq)]
enum Mode {
    Arguments,
    Command,
    Inputs,
    Files
}


impl Args {
    pub fn parse(&mut self) -> Result<(), ParseErr> {
        let mut raw_args = env::args().skip(1).peekable();
        let mut comm = String::with_capacity(128);
        let mut lists: Vec<Vec<String>>= Vec::new();
        let mut current_inputs: Vec<String> = Vec::new();

        // The purpose of this is to set the initial parsing mode.
        let mut mode = match raw_args.peek().unwrap().as_ref() {
            ":::"  => Mode::Inputs,
            "::::" => Mode::Files,
            _      => Mode::Arguments
        };

        // If there are no arguments to be parsed, then the inputs are commands.
        self.inputs_are_commands = mode == Mode::Inputs || mode == Mode::Files;

        // Parse each and every input argument supplied to the program.
        while let Some(argument) = raw_args.next() {
            let argument = argument.as_str();
            match mode {
                Mode::Arguments => {
                    let mut char_iter = argument.chars().peekable();

                    // If the first character is a '-' then it will be processed as an argument.
                    // We can guarantee that there will always be at least one character.
                    if char_iter.next().unwrap() == '-' {
                        // If the second character exists, everything's OK.
                        if let Some(character) = char_iter.next() {
                            // This scope of code allows users to utilize the GNU style
                            // command line arguments, to allow for laziness.
                            if character == 'j' {
                                // The short-hand job argument needs to be handled specially.
                                self.ncores = if char_iter.peek().is_some() {
                                    // Each character that follows after `j` will be considered an
                                    // input value.
                                    try!(jobs::parse(&argument[2..]))
                                } else {
                                    // If there was no character after `j`, the following argument
                                    // must be the job value.
                                    let ref val = try!(raw_args.next().ok_or(ParseErr::JobsNoValue));
                                    try!(jobs::parse(val))
                                }
                            } else if character != '-' {
                                // All following characters will be considered their own argument.
                                let mut char_iter = argument[1..].chars();
                                while let Some(character) = char_iter.next() {
                                    match character {
                                        'h' => {
                                            println!("{}", man::MAN_PAGE);
                                            exit(0);
                                        },
                                        'n' => self.uses_shell = false,
                                        'u' => self.grouped = false,
                                        'v' => self.verbose = true,
                                        _ => {
                                            return Err(ParseErr::InvalidArgument(argument.to_owned()))
                                        }
                                    }
                                }
                            } else {
                                // These are all the long mode versions of the arguments.
                                match &argument[2..] {
                                    "help" => {
                                        println!("{}", man::MAN_PAGE);
                                        exit(0);
                                    },
                                    "jobs" => {
                                        let ref val = try!(raw_args.next().ok_or(ParseErr::JobsNoValue));
                                        self.ncores = try!(jobs::parse(val))
                                    },
                                    "ungroup" => self.grouped = false,
                                    "no-shell" => self.uses_shell = false,
                                    "num-cpu-cores" => {
                                        println!("{}", num_cpus::get());
                                        exit(0);
                                    },
                                    "verbose" => self.verbose = true,
                                    _ => {
                                        return Err(ParseErr::InvalidArgument(argument.to_owned()));
                                    }
                                }
                            }
                        } else {
                            // `-` will never be a valid argument
                            return Err(ParseErr::InvalidArgument("-".to_owned()));
                        }
                    } else {
                        match argument {
                            ":::" => {
                                mode = Mode::Inputs;
                                self.inputs_are_commands = true;
                            },
                            "::::" => {
                                mode = Mode::Files;
                                self.inputs_are_commands = true;
                            }
                            _ => {
                                // The command has been supplied, and argument parsing is over.
                                comm.push_str(argument);
                                mode = Mode::Command;
                            }
                        }
                    }
                },
                Mode::Command => match argument {
                    // Arguments after `:::` are input values.
                    ":::" | ":::+" => mode = Mode::Inputs,
                    // Arguments after `::::` are files with inputs.
                    "::::" | "::::+" => mode = Mode::Files,
                    // All other arguments are command arguments.
                    _ => {
                        comm.push(' ');
                        comm.push_str(&argument);
                    }
                },
                _ => match argument {
                    ":::"  => {
                        mode = Mode::Inputs;
                        if !current_inputs.is_empty() {
                            lists.push(current_inputs.clone());
                            current_inputs.clear();
                        }
                    },
                    ":::+" => mode = Mode::Inputs,
                    "::::"  => {
                        mode = Mode::Files;
                        if !current_inputs.is_empty() {
                            lists.push(current_inputs.clone());
                            current_inputs.clear();
                        }
                    },
                    "::::+" => mode = Mode::Files,
                    _ => match mode {
                        Mode::Inputs => current_inputs.push(argument.to_owned()),
                        Mode::Files => try!(file_parse(&mut current_inputs, argument)),
                        _ => unreachable!()
                    }
                }
            }
        }

        tokenize(&mut self.arguments, &comm);

        if !current_inputs.is_empty() {
            lists.push(current_inputs.clone());
        }

        if lists.len() > 1 {
            // Convert the Vec<Vec<String>> into a Vec<Vec<&str>>
            let tmp: Vec<Vec<&str>> = lists.iter()
                .map(|list| list.iter().map(AsRef::as_ref).collect::<Vec<&str>>())
                .collect();

            // Convert the Vec<Vec<&str>> into a Vec<&[&str]>
            let list_array: Vec<&[&str]> = tmp.iter().map(AsRef::as_ref).collect();

            // Create a `Permutator` with the &[&[&str]] as the input.
            let permutator = Permutator::new(&list_array[..])
                // Have the permutator produce space-delimited strings with the permutations.
                .map(|permutation| {
                    let mut iter = permutation.iter();
                    let mut output = String::from(*iter.next().unwrap());
                    for element in iter {
                        output.push(' ');
                        output.push_str(element);
                    }
                    output
                });

            for permutation in permutator {
                self.inputs.push(permutation)
            }
        } else {
            self.inputs = current_inputs;
        }

        // If no inputs are provided, read from stdin instead.
        if self.inputs.is_empty() {
            let stdin = io::stdin();
            for line in stdin.lock().lines() {
                if let Ok(line) = line { self.inputs.push(line) }
            }
        }

        Ok(())
    }
}

/// Attempts to open an input argument and adds each line to the `inputs` list.
fn file_parse(inputs: &mut Vec<String>, path: &str) -> Result<(), ParseErr> {
    let file = try!(fs::File::open(path)
        .map_err(|err| ParseErr::InputFileError(path.to_owned(), err.to_string())));
    for line in BufReader::new(file).lines() {
        if let Ok(line) = line { inputs.push(line); }
    }
    Ok(())
}