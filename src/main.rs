extern crate num_cpus;

use std::env;
use std::io::{self, Write, StderrLock};
use std::process::{Command, exit};
use std::thread::{self, JoinHandle};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/* TODO: Functionality can be increased to accept the following syntaxes from GNU Parallel:
 - Stdin support is currently missing.
 - {N}, {N.}, etc.
 - parallel command {1} {2} {3} ::: 1 2 3 ::: 4 5 6 ::: 7 8 9
 - paralllel command ::: a b c :::+ 1 2 3 ::: d e f :::+ 4 5 6
*/

fn main() {
    let stderr = io::stderr();
    let mut ncores = num_cpus::get();
    let mut command = String::new();
    let mut arg_tokens = Vec::new();
    let mut inputs = Vec::new();

    // Let's collect all parameters that we need from the program's arguments.
    // If an error is returned, this will handle that error as efficiently as possible.
    if let Err(why) = parse_arguments(&mut ncores, &mut command, &mut arg_tokens, &mut inputs) {
        let mut stderr = stderr.lock();
        let _ = stderr.write(b"parallel: parsing error: ");
        match why {
            ParseErr::JobsNaN(value) => {
                let _ = stderr.write(b"jobs parameter, '");
                let _ = stderr.write(value.as_bytes());
                let _ = stderr.write(b"', is not a number.\n");
            },
            _ => {
                let message: &[u8] = match why {
                    ParseErr::InputVarsNotDefined => b"input variables were not defined.\n",
                    ParseErr::JobsNoValue         => b"no jobs parameter was defined.\n",
                    _ => unreachable!()
                };
                let _ = stderr.write(message);
            }
        };
        exit(1);
    }

    // If no command was given, then the inputs are actually commands themselves.
    let input_is_command = command.is_empty();

    // It will be useful to know the number of inputs, to know when to quit.
    let num_inputs = inputs.len();

    // Stores the next input to be processed
    let shared_counter = Arc::new(AtomicUsize::new(0));

    // We will share the same list of inputs with each thread.
    let shared_input = Arc::new(inputs);

    // First we will create as many threads as `ncores` specifies.
    // The `threads` vector will contain the thread handles needed to
    // know when to quit the program.
    let mut threads: Vec<JoinHandle<()>> = Vec::with_capacity(ncores);
    for slot in 1..ncores+1 {
        // The command that each input variable will be sent to.
        let command = command.clone();
        // The arguments for the command.
        let argument_tokens = arg_tokens.clone();
        // Allow the thread to gain access to the list of inputs.
        let input = shared_input.clone();
        // Allow the thread to access the current command counter
        let counter = shared_counter.clone();
        // Allow the thread to know when it's time to stop.
        let num_inputs = num_inputs;

        // The actual thread where the work will happen on incoming data.
        let handle: JoinHandle<()> = thread::spawn(move || {
            let slot_number = slot;
            let stderr = io::stderr();
            loop {
                // Obtain the Nth item and it's job ID from the list of inputs.
                let (input_var, job_id) = {
                    // Atomically increment the counter
                    let old_counter = counter.fetch_add(1, Ordering::SeqCst);
                    if old_counter >= num_inputs {
                        break
                    } else {
                        let input_var = &input[old_counter];
                        let job_id = old_counter + 1;
                        (input_var, job_id)
                    }
                };

                if input_is_command {
                    // The inputs are actually the commands.
                    let mut iterator = input_var.split_whitespace();
                    let actual_command = iterator.next().unwrap();
                    let args = iterator.collect::<Vec<&str>>();
                    if let Err(_) = Command::new(actual_command).args(&args).status() {
                        let mut stderr = stderr.lock();
                        let _ = stderr.write(b"parallel: command error: ");
                        let _ = stderr.write(input_var.as_bytes());
                        let _ = stderr.write(b"\n");
                    }
                } else {
                    // Build a command by merging the command template with the input,
                    // and then execute that command.
                    let (slot, job) = (slot_number.to_string(), job_id.to_string());
                    if let Err(cmd_err) = cmd_builder(input_var, &command, &argument_tokens,
                        &slot, &job)
                    {
                        let mut stderr = stderr.lock();
                        cmd_err.handle(&mut stderr);
                    }
                }
            }
        });

        // After the thread has been created, add the important pieces needed by the
        // main thread to the `threads` vector.
        threads.push(handle);
    }

    for thread in threads.into_iter() { thread.join().unwrap(); }
}

enum CommandErr {
    Failed(String, Vec<String>)
}

impl CommandErr {
    fn handle(self, stderr: &mut StderrLock) {
        let _ = stderr.write(b"parallel: command error: ");
        match self {
            CommandErr::Failed(command, arguments) => {
                let _ = stderr.write(command.as_bytes());
                for arg in &arguments {
                    let _ = stderr.write(b" ");
                    let _ = stderr.write(arg.as_bytes());
                }
                let _ = stderr.write(b"\n");
            }
        }
    }
}

#[derive(Clone, PartialEq)]
enum Token {
    Character(char),
    Placeholder,
    RemoveExtension,
    Basename,
    Dirname,
    BaseAndExt,
    Slot,
    Job
}

fn tokenize(template: &str) -> Vec<Token> {
    let mut matching = false;
    let mut tokens = Vec::new();
    let mut pattern = String::new();
    for character in template.chars() {
        match (character, matching) {
            ('{', false) => matching = true,
            ('}', true)  => {
                matching = false;
                if pattern.is_empty() {
                    tokens.push(Token::Placeholder);
                } else {
                    match match_token(&pattern) {
                        Some(token) => tokens.push(token),
                        None => {
                            tokens.push(Token::Character('{'));
                            for character in pattern.chars() {
                                tokens.push(Token::Character(character));
                            }
                            tokens.push(Token::Character('}'));
                        }
                    }
                    pattern.clear();
                }
            }
            (_, false)  => tokens.push(Token::Character(character)),
            (_, true) => pattern.push(character)
        }
    }
    tokens
}

fn match_token(pattern: &str) -> Option<Token> {
    match pattern {
        "."  => Some(Token::RemoveExtension),
        "#"  => Some(Token::Job),
        "%"  => Some(Token::Slot),
        "/"  => Some(Token::Basename),
        "//" => Some(Token::Dirname),
        "/." => Some(Token::BaseAndExt),
        _    => None
    }

}

/// Builds the command and executes it
fn cmd_builder(input: &str, command: &str, arg_tokens: &[Token], slot_id: &str, job_id: &str)
    -> Result<(), CommandErr>
{
    let mut arguments = String::new();
    for arg in arg_tokens {
        match *arg {
            Token::Character(arg) => arguments.push(arg),
            Token::Basename => arguments.push_str(input),
            Token::BaseAndExt => arguments.push_str(basename(remove_extension(input))),
            Token::Dirname => arguments.push_str(dirname(input)),
            Token::Job => arguments.push_str(job_id),
            Token::Placeholder => arguments.push_str(input),
            Token::RemoveExtension => arguments.push_str(remove_extension(input)),
            Token::Slot => arguments.push_str(slot_id)
        }
    }

    let placeholder_exists = arg_tokens.iter().any(|ref x| {
        x == &&Token::BaseAndExt || x == &&Token::Basename || x == &&Token::Dirname ||
        x == &&Token::Job || x == &&Token::Placeholder || x == &&Token::RemoveExtension ||
        x == &&Token::Slot
    });

    if !placeholder_exists {
        arguments.push_str(input);
    }

    let arguments = arguments.split_whitespace().map(|x| x.to_owned()).collect::<Vec<String>>();

    if let Err(_) = Command::new(&command).args(&arguments).status() {
        return Err(CommandErr::Failed(String::from(command), arguments));
    }
    Ok(())
}

/// Removes the extension of a given input
fn remove_extension(input: &str) -> &str {
    let mut index = 0;
    for (id, character) in input.chars().enumerate() {
        if character == '.' { index = id; }
    }
    if index == 0 { input } else { &input[0..index] }
}

fn basename(input: &str) -> &str {
    let mut index = 0;
    for (id, character) in input.chars().enumerate() {
        if character == '/' { index = id; }
    }
    if index == 0 { input } else { &input[index+1..] }
}

fn dirname(input: &str) -> &str {
    let mut index = 0;
    for (id, character) in input.chars().enumerate() {
        if character == '/' { index = id; }
    }
    if index == 0 { input } else { &input[0..index] }
}

enum ParseErr {
    JobsNaN(String),
    JobsNoValue,
    InputVarsNotDefined,
}

// Parses input arguments and stores their values into their associated variabless.
fn parse_arguments(ncores: &mut usize, command: &mut String, arg_tokens: &mut Vec<Token>,
    input_variables: &mut Vec<String>) -> Result<(), ParseErr>
{
    let mut parsing_arguments = true;
    let mut command_is_set    = false;
    let mut raw_args = env::args().skip(1).peekable();
    let mut comm = String::new();
    while let Some(argument) = raw_args.next() {
        if parsing_arguments {
            match argument.as_str() {
                // Defines the number of jobs to run in parallel.
                "-j"  => {
                    match raw_args.peek() {
                        Some(val) => match val.parse::<usize>() {
                            Ok(val) => *ncores = val,
                            Err(_)  => return Err(ParseErr::JobsNaN(val.clone()))
                        },
                        None => return Err(ParseErr::JobsNoValue)
                    }
                    let _ = raw_args.next();
                },
                // Arguments after `:::` are input values.
                ":::" => parsing_arguments = false,
                _ => {
                    if command_is_set {
                        comm.push(' ');
                        comm.push_str(&argument);
                    } else {
                        comm.push_str(&argument);
                        command_is_set = true;
                    }

                }
            }
        } else {
            input_variables.push(argument);
        }
    }

    // This will fill in command and argument information needed by the threads.
    // If there is a space in the argument, then the command has arguments
    match comm.chars().position(|x| x == ' ') {
        Some(pos) => {
            *command    = String::from(&comm[0..pos]);
            *arg_tokens = tokenize(&comm[pos+1..]);
        },
        None => *command = comm
    }

    if input_variables.is_empty() { return Err(ParseErr::InputVarsNotDefined) }
    Ok(())
}