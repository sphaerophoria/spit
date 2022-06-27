use anyhow::{bail, Context, Result};
use log::error;
use std::{
    path::{Path, PathBuf},
    io::{BufRead, Read, Write, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, mpsc::{self, Sender, Receiver}},
    thread::{self, JoinHandle},
    ffi::OsString, time::Duration,
};

pub(crate) struct InteractiveCommandRunner 
{
    // NOTE: We could run the command in the main thread, however if we want to keep sending output
    // back to the UI thread we either have to poll the output from the parent class or spin up a
    // thread to manage it here.
    current_child: Option<Child>,
    on_output: Arc<dyn Fn(String) + Send + Sync>,
}

impl InteractiveCommandRunner
{
    pub(crate) fn new<F>(on_output: F) -> Result<Self> 
        where F: Fn(String) + Send + Sync + 'static
    {
       Ok(Self {
            current_child: None,
            on_output: Arc::new(on_output),
        })
    }

    pub(crate) fn spawn(&mut self, s: &str, cwd: &Path) -> Result<()> {
        if self.is_running() {
            bail!("Unable to spawn process when one is already running");
        }

        let mut child = run_command(s, cwd)
            .context("Failed to spawn command")?;

        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();

        thread::spawn({
            let on_output = Arc::clone(&self.on_output);
            move || {
                println!("Waiting for stdout");
                let stdout = BufReader::new(stdout);
                let mut bytes = stdout.bytes();
                while let Some(Ok(b)) = bytes.next() {
                    println!("Byte baybee");
                    if let Ok(s) = std::str::from_utf8(&[b]) {
                        on_output(s.to_string());
                    } else {
                        on_output("Invalid UTF8 from command line".to_string());
                    }

                }
        }});

        //thread::spawn({
        //    let on_output = Arc::clone(&self.on_output);
        //    move || {
        //        let mut buf = [0; 1024];
        //        while let Ok(n) = stderr.read(&mut buf) {
        //            if n == 0 {
        //                continue;
        //            }
        //            if let Ok(s) = std::str::from_utf8(&buf[0..n]) {
        //                on_output(s.to_string());
        //            } else {
        //                on_output("Invalid UTF8 from command line".to_string());
        //            }

        //        }
        //}});


        self.current_child = Some(child);
        Ok(())
    }

    /// Sometimes it's unknown if the input is stdin for a running command, or a new command to
    /// run. E.g. User input. This will run a command if no command is running, or send to stdin
    /// if it is
    pub(crate) fn push(&mut self, s: &str, cwd: &Path) -> Result<()> {
        if self.is_running() {
            let child = self.current_child.as_mut().unwrap();
            let stdin = child.stdin.as_mut().unwrap();

            println!("Writing to stdin: {}", s);

            writeln!(stdin, "{}", s)
                .context("Failed to write to stdin")?;
        } else {
            self.spawn(s, cwd)?;
        }

        Ok(())
    }

    fn is_running(&mut self) -> bool {
        match &mut self.current_child {
            Some(child) => {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        child.wait().unwrap();
                        false
                    },
                    Ok(None) => true,
                    Err(_) => false,
                }
            }
            None => false,
        }
    }
}

fn run_command(cmd: &str, cwd: &Path) -> Result<Child> {
        let mut bash_cmd = OsString::new();
        bash_cmd.push(&cmd);
        bash_cmd.push(" 2>&1");

        let editor = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|x| x.to_path_buf()))
            .map(|p| p.join("spit-editor"));

        let mut command = Command::new("/bin/bash");

        command.arg("-c").arg(bash_cmd).current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped());

        if let Some(editor) = editor {
            command.env("EDITOR", editor);
        }

        command.spawn().map_err(From::from)
}

