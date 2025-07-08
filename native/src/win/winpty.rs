#![deny(clippy::all)]
/// Copyright (c) 2013-2015, Christopher Jeffrey, Peter Sunde (MIT License)
/// Copyright (c) 2016, Daniel Imms (MIT License).
/// Copyright (c) 2018, Microsoft Corporation (MIT License).
/// Copyright (c) 2022, Daniel Brenot, Shachar Itzhaky (MIT License)
/// 
/// This file is responsible for starting processes
/// with pseudo-terminal file descriptors.

use std::collections::HashMap;

use crate::{err};

#[cfg(target_family = "windows")] use {
    crate::util::map_to_wstring,
    lazy_static::lazy_static,
    std::{ sync::{Arc, Mutex}, ptr::{null_mut, null} },
    std::os::raw::c_int,
    std::collections::hash_map::Entry::{Vacant, Occupied},
    which::which,
    winsafe::WString,
    windows::{Win32::{System::{Environment::SetEnvironmentVariableW, Threading::{GetProcessId, GetExitCodeProcess}}, Foundation::{HANDLE, CloseHandle}}, core::PCWSTR},
    winpty_sys::{winpty_config_new, winpty_error_ptr_t, winpty_error_free, winpty_config_set_initial_size, winpty_open, winpty_config_free, winpty_t, winpty_agent_process, winpty_spawn_config_new, WINPTY_SPAWN_FLAG_AUTO_SHUTDOWN, winpty_spawn, winpty_spawn_config_free, winpty_conin_name, winpty_conout_name, winpty_set_size, winpty_get_console_process_list, DWORD}
};

#[cfg(target_family = "windows")]
const WINPTY_DBG_VARIABLE: &str = "WINPTY_DEBUG";

#[cfg(target_family = "windows")]
lazy_static! {
    /// Static list of all pty handles protected by a mutex and atomic ref count 
    static ref PTY_HANDLES: Arc<Mutex<HashMap<usize, winpty_t>>> =  Arc::new(Mutex::new(HashMap::new()));
}

#[napi(object)]
pub struct IWinptyProcess {
    pub pty: i32,
    pub fd: i32,
    pub conin: String,
    pub conout: String,
    pub pid: i32,
    pub inner_pid: i32,
    pub inner_pid_handle: i32
}

#[allow(dead_code, unused_variables)]
#[napi]
unsafe fn winpty_start_process(mut file: String, command_line: String, env: HashMap<String, String>, cwd: String, cols: i32, rows: i32, debug: bool) -> napi::Result<IWinptyProcess> {
    #[cfg(not(target_family = "windows"))]
    return err!("Unsupported architecture");
    #[cfg(target_family = "windows")] {
        // Convert map to a string suitable for native apis
        let env = map_to_wstring(env);

        // Make sure the file exists
        match which(file) {
            Ok(f) => file = String::from(f.as_path().to_str().expect("Failed to convert file path to string")),
            Err(e) => return err!(format!("File not found: {}", e.to_string())),
        }
        // Convert arguments to native strings for c ffi
        let file = WString::from_str(file.as_str());
        let command_line = WString::from_str(command_line.as_str());
        let cwd = WString::from_str(cwd.as_str());

        // Enable/disable debugging
        SetEnvironmentVariableW(
            PCWSTR(WString::from_str(WINPTY_DBG_VARIABLE).as_ptr()),
            if debug { PCWSTR(WString::from_str("1").as_ptr()) } else { PCWSTR(null())});

        // Pointer to store error from last opperation of winpty
        let mut error_ptr: winpty_error_ptr_t = null_mut();

        let winpty_config = winpty_config_new(0, &mut error_ptr);
        if winpty_config.is_null() {
            winpty_error_free(error_ptr);
            return err!("Error creating WinPTY config");
        }
        winpty_error_free(error_ptr);

        // Set pty size on config
        winpty_config_set_initial_size(winpty_config, cols, rows);

        // Start the pty agent
        let pc = winpty_open(winpty_config, &mut error_ptr);
        winpty_config_free(winpty_config);
        if pc.is_null() {
            winpty_error_free(error_ptr);
            return err!("Error launching WinPTY agent");
        }
        winpty_error_free(error_ptr);

        // Process id for agent process. Can be used to get handle later
        let pid = winpty_agent_process(pc) as usize;
        // Using another scope allows the lock to be immediately released once
        // it is no longer used. Alternatively, it can be released with free
        {
            // Save pty struct for later use
            let mut handles = PTY_HANDLES.lock().unwrap();
            handles.entry(pid).insert_entry(*pc);
        }

        // Create winpty spawn config
        let config = winpty_spawn_config_new(
            WINPTY_SPAWN_FLAG_AUTO_SHUTDOWN as _,
            file.as_ptr(),
            command_line.as_ptr(),
            cwd.as_ptr(),
            env.as_ptr(),
            &mut error_ptr
        );
        if config.is_null() {
            winpty_error_free(error_ptr);
            return err!("Error creating WinPTY spawn config");
        }
        winpty_error_free(error_ptr);

        // Spawn the new process
        let handle = HANDLE::default();
        let spawn_success = winpty_spawn(
            pc,
            config,
            handle.0 as _,
            null_mut(),
            null_mut(),
            &mut error_ptr
        ) != 0;
        winpty_spawn_config_free(config);
        if !spawn_success {
            winpty_error_free(error_ptr);
            return err!("Unable to start terminal process");
        }
        winpty_error_free(error_ptr);

        // Get the input and output connection names for the pty session
        let conin = WString::from_wchars_nullt(winpty_conin_name(pc)).to_string();
        let conout = WString::from_wchars_nullt(winpty_conout_name(pc)).to_string();

        // We return the pid as a pty since it is used to index the proces from the map
        Ok(IWinptyProcess {
            inner_pid: GetProcessId(handle) as _,
            inner_pid_handle: handle.0 as _,
            pid: pid as _,
            pty: pid as _,
            conin,
            conout,
            fd: -1,
        })
    }
}

#[allow(dead_code, unused_variables)]
#[napi]
fn winpty_resize(pid: i32, cols: i32, rows: i32) -> napi::Result<()> {
    #[cfg(not(target_family = "windows"))]
    return err!("Unsupported architecture");
    #[cfg(target_family = "windows")] {
        let mut handles = PTY_HANDLES.lock().unwrap();

        let pc = handles.entry(pid as _);
        match pc {
            Occupied(mut handle_entry) => {
                if unsafe { winpty_set_size(handle_entry.get_mut() as _, cols, rows, null_mut()) != 0 } {
                    return err!("The pty could not be resized");
                }
                return Ok(());

            },
            Vacant(_) => {
                return err!("Pty seems to have been killed already");
            },
        }
    }
}

#[allow(dead_code, unused_variables)]
#[napi]
fn winpty_kill(pid: i32, inner_pid_handle: i32) -> napi::Result<()> {
    #[cfg(not(target_family = "windows"))]
    return err!("Unsupported architecture");
    #[cfg(target_family = "windows")] {
        let mut handles = PTY_HANDLES.lock().unwrap();

        let pc = handles.entry(pid as _);
        match pc {
            Occupied(handle_entry) => {
                handle_entry.remove_entry();
                unsafe { CloseHandle(HANDLE(inner_pid_handle as _)); }
                return Ok(());
            },
            Vacant(_) => {
                return err!("Pty seems to have been killed already");
            },
        }
    }
}

#[allow(dead_code, unused_variables)]
#[napi]
fn winpty_get_process_list(pid: i32) -> napi::Result<Vec<i32>> {
    #[cfg(not(target_family = "windows"))]
    return err!("Unsupported architecture");
    #[cfg(target_family = "windows")] {
        let mut handles = PTY_HANDLES.lock().unwrap();

        let pc = handles.entry(pid as _);
        match pc {
            Occupied(mut handle_entry) => {
                unsafe {
                    // The count of how many 
                    const PROCESS_COUNT: c_int = 64;
                    // Create an array to be filled by the native system call
                    let mut process_list = [0 as c_int; PROCESS_COUNT as usize];
                    let actual_count = winpty_get_console_process_list(
                        handle_entry.get_mut(),
                        &mut process_list as _,
                        PROCESS_COUNT,
                        null_mut()
                    );
                    let mut process_list = Vec::<i32>::with_capacity(actual_count as usize);
                    for i in 0..actual_count {
                        process_list.push(*process_list.get_unchecked(i as usize));
                    }
                    return Ok(process_list);
                }
            },
            Vacant(_) => {
                return err!("Pty seems to have been killed already");
            },
        }
    }
}

#[allow(dead_code, unused_variables)]
#[napi]
fn winpty_get_exit_code(inner_pid_handle: i32) -> i32 {
    #[cfg(not(target_family = "windows"))]
    return -1;
    #[cfg(target_family = "windows")] {
        let mut exit_code: DWORD = 0;
        unsafe { 
            GetExitCodeProcess(
            HANDLE(inner_pid_handle as isize),
            &mut exit_code
            );
            return exit_code as _;
        }
    }
}
