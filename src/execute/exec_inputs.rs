use arguments;
use command;
use wait_timeout::ChildExt;
use super::pipe::disk::output as pipe_output;
use super::pipe::disk::State;
use super::super::input_iterator::InputIterator;
use super::super::shell;
use super::attempt_next;
use verbose;

use std::time::Duration;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::Sender;

pub struct ExecInputs {
    pub num_inputs: usize,
    pub delay:      Duration,
    pub timeout:    Duration,
    pub inputs:     Arc<Mutex<InputIterator>>,
    pub output_tx:  Sender<State>,
}

impl ExecInputs {
    pub fn run(&self, mut flags: u16) {
        let stdout = io::stdout();
        let stderr = io::stderr();

        let job_total   = &self.num_inputs.to_string();
        let has_delay   = self.delay != Duration::from_millis(0);
        let has_timeout = self.timeout != Duration::from_millis(0);
        let mut completed = false;

        while let Some((input, job_id, _)) = attempt_next(&self.inputs, &stderr, has_delay, self.delay,
            &mut completed, flags)
        {
            if flags & arguments::VERBOSE_MODE != 0 {
                verbose::processing_task(&stdout, &job_id.to_string(), job_total, &input);
            }

            if shell::required(shell::Kind::Input(&input)) {
                if shell::dash_exists() {
                    flags |= arguments::DASH_EXISTS + arguments::SHELL_ENABLED;
                } else {
                    flags |= arguments::SHELL_ENABLED;
                }
            }

            match command::get_command_output(&input, flags) {
                Ok(mut child) => {
                    if has_timeout && child.wait_timeout(self.timeout).unwrap().is_none() {
                        let _ = child.kill();
                        pipe_output(&mut child, job_id, input.clone(), &self.output_tx, flags & arguments::QUIET_MODE != 0);
                    } else {
                        pipe_output(&mut child, job_id, input.clone(), &self.output_tx, flags & arguments::QUIET_MODE != 0);
                        let _ = child.wait();
                    }
                },
                Err(why) => {
                    let mut stderr = stderr.lock();
                    let _ = write!(&mut stderr, "parallel: command error: {}: {}\n", input, why);
                    let message = format!("{}: {}: {}\n", job_id, input, why);
                    let _ = self.output_tx.send(State::Error(job_id, message));
                }
            }

            if flags & arguments::VERBOSE_MODE != 0 {
                verbose::task_complete(&stdout, &job_id.to_string(), job_total, &input);
            }
        }
    }
}