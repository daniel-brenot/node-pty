#![deny(clippy::all)]
// Copyright (c) 2013-2015, Christopher Jeffrey, Peter Sunde (MIT License)
// Copyright (c) 2016, Daniel Imms (MIT License).
// Copyright (c) 2018, Microsoft Corporation (MIT License).
// Copyright (c) 2022, Daniel Brenot, Shachar Itzhaky (MIT License)
//
// This file is responsible for starting processes
// with pseudo-terminal file descriptors.


use crate::err;
use std::collections::HashMap;
use napi::JsFunction;
#[cfg(target_family = "windows")] use {
  std::ptr::null_mut,
  std::sync::{Arc, Mutex, atomic::AtomicUsize},
  lazy_static::lazy_static,
  napi::{
    threadsafe_function::{
      ThreadSafeCallContext, ErrorStrategy,
      ThreadsafeFunction, ErrorStrategy::Fatal,
      ThreadsafeFunctionCallMode
    }
  },
  windows::{
    Win32::{
      Foundation::{
        HANDLE, INVALID_HANDLE_VALUE,
        BOOLEAN, CloseHandle, GetLastError
      },
      System::{
        Threading::{
          RegisterWaitForSingleObject, WT_EXECUTEONLYONCE,
          PROCESS_INFORMATION, STARTUPINFOW,
          STARTUPINFOEXW, STARTF_USESTDHANDLES,
          LPPROC_THREAD_ATTRIBUTE_LIST,
          InitializeProcThreadAttributeList,
          UpdateProcThreadAttribute,
          PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
          CreateProcessW,
          EXTENDED_STARTUPINFO_PRESENT,
          CREATE_UNICODE_ENVIRONMENT,
          GetExitCodeProcess,
          ResumeThread,
          CREATE_SUSPENDED
        },
        WindowsProgramming::INFINITE,
        Console::{
          COORD, HPCON, SetConsoleCtrlHandler,
          CreatePseudoConsole, ResizePseudoConsole, ClosePseudoConsole
        },
        Pipes::{
          CreateNamedPipeW, PIPE_TYPE_BYTE,
          PIPE_READMODE_BYTE, PIPE_WAIT,
          ConnectNamedPipe, DisconnectNamedPipe
        }
      },
      Storage::FileSystem::{
        PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND,
        FILE_FLAG_FIRST_PIPE_INSTANCE
      },
      Security::SECURITY_ATTRIBUTES
    },
    core::{PCWSTR, PWSTR}
  },
  crate::{util::{map_to_wstring, coerce_i16}},
  winsafe::{WString}
};

#[cfg(target_family = "windows")]
struct ConptyBaton {
  /// Handle for the pseudoconsole
  pub hpc: HPCON,
  /// Handle for the input pipe
  pub h_in: HANDLE,
  /// Handle for the output pipe
  pub h_out: HANDLE,
  /// Handle for the shell
  pub h_shell: Option<HANDLE>,
  /// Handle for process thread to start it
  pub h_thread: Option<HANDLE>,
  /// Handle for waiting on the shell exit
  pub h_wait: Option<HANDLE>,
  /// The callback to be called when the corresponding process exits
  pub async_cb: Option<ThreadsafeFunction<u32, Fatal>>
}

/// Allows for the baton to be sent across threads.
/// We need to make sure any pointers held here are released
/// to avoid memory leaks, and generally follow best practices
/// for sharing pointers across threads
#[cfg(target_family = "windows")]
unsafe impl Send for ConptyBaton {}

/// Static count of the next available id
#[cfg(target_family = "windows")]
static PTY_COUNT: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_family = "windows")]
lazy_static! {
  /// Static list of all pty handles protected by a mutex and atomic ref count
  static ref PTY_HANDLES: Arc<Mutex<HashMap<usize, ConptyBaton>>> =  Arc::new(Mutex::new(HashMap::new()));
}

/// Returns a new server named pipe.
/// It has not yet been connected.
#[cfg(target_family = "windows")]
pub unsafe fn create_data_server_pipe(pipe_name: WString) -> napi::Result<HANDLE> {
  let win_open_mode = PIPE_ACCESS_INBOUND | PIPE_ACCESS_OUTBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE;
  // Initialize empty security attributes
  let mut sa: SECURITY_ATTRIBUTES = SECURITY_ATTRIBUTES::default();
  sa.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as _;
  let h_server = CreateNamedPipeW(
    PCWSTR(pipe_name.as_ptr()), win_open_mode,
    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
    1, 0, 0, 30000,
    &sa
  );
  if h_server == INVALID_HANDLE_VALUE {
    return err!(format!("Failed to create handle for pipe. Error code: {}", GetLastError().0));
  }
  Ok(h_server)
}

/// Creates input and output pipes with the given name
#[cfg(target_family = "windows")]
pub unsafe fn create_named_pipes_and_pseudo_console(
  size: COORD,
  flags: u32,
  pipe_name: String
) -> napi::Result<IConptyProcess> {
  // Create names for the input and output pipes
  let name_input = format!("\\\\.\\pipe\\{}-in", pipe_name);
  let name_output = format!("\\\\.\\pipe\\{}-out", pipe_name);
  // Creates the input side of the pipe
  let ph_input = create_data_server_pipe(WString::from_str(&name_input))?;
  // Creates the output side of the pipe
  let ph_output = create_data_server_pipe(WString::from_str(&name_output))?;
  // Creates a pseudoconsole using the input and output sides of the pipes
  match CreatePseudoConsole(size, ph_input, ph_output, flags) {
    Ok(hpc) => {
      // Creates a new baton and adds it to the global store.
      // This baton doesn't have a handle for the process
      // or a callback yet because they still need to be initialized with
      // a call to conpty_connect
      let baton = ConptyBaton {
        hpc,
        h_in: ph_input,
        h_out: ph_output,
        async_cb: None,
        h_shell: None,
        h_thread: None,
        h_wait: None
      };
      let id = PTY_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
      // Add the baton to the list
      PTY_HANDLES.lock().unwrap().entry(id).insert_entry(baton);
      Ok(IConptyProcess {
        fd: -1, pty: id as _,
        conin: name_input, conout: name_output
      })
    },
    Err(_) => { err!("Failed to create Pseudoconsole") }
  }
}

/// Creates a thread with the provided commandline, directory and environment.
///
/// Returns the process information for the newly created thread.
#[cfg(target_family = "windows")]
pub unsafe fn pty_connect(
  id: usize,
  cmdline: String,
  cwd: String,
  env: HashMap<String, String>,
  onexit: JsFunction
) -> napi::Result<PROCESS_INFORMATION> {
  // Convert all 3 values to wstrings

  use std::{ptr, alloc::Layout};
  use windows::Win32::Foundation::ERROR_PIPE_CONNECTED;

  let env = map_to_wstring(env);
  let mut cmdline = WString::from_str(cmdline.as_str());
  let mut cwd = WString::from_str(cwd.as_str());

  // Get the global handles
  let mut handles = PTY_HANDLES.lock().unwrap();
  // Fetch pty handle from ID and start process
  match handles.get_mut(&id) {
    Some(baton) => {
      // Connects the named input and output pipes
      let mut success = ConnectNamedPipe(baton.h_in, null_mut()).as_bool()
        && ConnectNamedPipe(baton.h_out, null_mut()).as_bool();
      // We ignore this error since it's actually a good thing!
      if !success && GetLastError() != ERROR_PIPE_CONNECTED { return err!(format!("Failed to connect named pipes. Error code: {}", GetLastError().0)); }

      let mut lpstartupinfo = STARTUPINFOW::default();
      lpstartupinfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as _;
      lpstartupinfo.dwFlags = STARTF_USESTDHANDLES;

      // Attach the pseudoconsole to the client application we're creating
      let mut si_ex = STARTUPINFOEXW::default();
      si_ex.StartupInfo = lpstartupinfo;

      let mut size: usize = 0;
      // Passing a null list allows for getting the appropriate size of the attribute list
      let null_list = LPPROC_THREAD_ATTRIBUTE_LIST(ptr::null_mut());
      InitializeProcThreadAttributeList(null_list, 1, 0, &mut size);

      // BYTE *attrList = new BYTE[size];
      let attr_list = std::alloc::alloc(Layout::from_size_align(size, std::mem::align_of::<u8>()).unwrap());
      si_ex.lpAttributeList = LPPROC_THREAD_ATTRIBUTE_LIST(attr_list as _);

      success = InitializeProcThreadAttributeList(si_ex.lpAttributeList, 1, 0, &mut size).as_bool();
      if !success { return err!(format!("InitializeProcThreadAttributeList failed. Error code: {}, bufferSize={}", GetLastError().0, size)); }

      success = UpdateProcThreadAttribute(
        si_ex.lpAttributeList,
        0,
        PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as _,
        baton.hpc.0 as _,
        std::mem::size_of::<HPCON>(),
        null_mut(),
        null_mut()
      ).as_bool();

      if !success { return err!("Failed to update thread attribute") }

      // Creates a process and gets the information about it to return
      let mut pi_client = PROCESS_INFORMATION::default();
      success = CreateProcessW(
        PCWSTR(null_mut()),
        PWSTR(cmdline.as_mut_ptr()),
        null_mut(),
        null_mut(),
        false,
        EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
        env.as_ptr() as _,
        PCWSTR(cwd.as_mut_ptr()),
        &si_ex.StartupInfo,
        &mut pi_client
      ).as_bool();

      if !success { return err!("Cannot create process"); }

      // Set the handle for the shell process and thread to the one obtained
      // from creating the new process
      baton.h_shell = Some(pi_client.hProcess);
      baton.h_thread = Some(pi_client.hThread);

      // Set the async callback for the baton to the function passed in from js
      baton.async_cb = Some(onexit.create_threadsafe_function::<_, _, _, ErrorStrategy::Fatal>(0,
         |ctx: ThreadSafeCallContext<u32>| {
           ctx.env.create_uint32(ctx.value).map(|v0| { vec![v0] })
         }
      )?);

      // Setup Windows wait for process exit event
      let mut h_wait = HANDLE::default();
      let handle_copy = pi_client.hProcess;
      RegisterWaitForSingleObject(
        &mut h_wait,
        pi_client.hProcess,
        Some(on_exit_process),
        Box::into_raw(Box::new(handle_copy)) as *mut _,
        INFINITE,
        WT_EXECUTEONLYONCE
      );
      baton.h_wait = Some(h_wait);

      Ok(pi_client)
    },
    None => err!(format!("Failed to find baton for id {}", id)),
  }
}

/// This function acts as a native callback point for when processes end.
///
/// This is marked as "extern" so that the callback isn't mangled
/// by the compiler. This allows for the other process to call a
/// predictable function when it exits. The exit code can then
/// be sent to the async callback in javascript
#[cfg(target_family = "windows")]
pub unsafe extern "system" fn on_exit_process(ctx: *mut std::ffi::c_void, _b: BOOLEAN) -> () {
  // Takes ownership of the raw pointer,
  // allowing it to be owned by the local context and
  // deallocated when it goes out of scope
  let ctx: Box<HANDLE> = Box::from_raw(ctx as _);

  // Get the global map of all handles
  let mut handles = PTY_HANDLES.lock().expect(
    "Failed to aquire lock for pty handles when exiting process"
  );

  if let Some((key, baton)) = handles.iter_mut().find(|(_key, value)| {
    value.h_shell == Some(*ctx)
  }) {
    let mut exit_code: u32 = 0;
    //We need to get the exit code before closing the process
    GetExitCodeProcess(*ctx, &mut exit_code);

    // Create a copy of the key to avoid borrowing issues
    let key = *key;

    // Check if the baton exists and has an async callback
    if let Some(cb) = baton.async_cb.take() {
      cb.call(exit_code, ThreadsafeFunctionCallMode::Blocking);
    }

    // Free the baton from the global map.
    handles.remove(&key);
  }
}


#[napi(object)]
pub struct IConptyProcess {
  pub fd: i32,
  pub pty: i32,
  pub conin: String,
  pub conout: String
}

#[napi(object)]
struct IConptyConnection {
  pub pid: u32,
  pub tid: u32
}

/// This function creates the pseudoconsole and the named pipes for input and output.
///
/// Returns the created process information.
#[allow(dead_code, unused_variables)]
#[napi(js_name = "conptyCreateProcess")]
fn conpty_create_process(
  cols: i32, rows: i32,
  pipe_name: String,
  conpty_inherit_cursor: bool
) -> napi::Result<IConptyProcess> {
  #[cfg(not(target_family = "windows"))]
  return err!("Platform not supported");
  #[cfg(target_family = "windows")] {
    // Coerce the input values into coordinate values
    let cols = coerce_i16(cols)?;
    let rows = coerce_i16(rows)?;

    unsafe {
      let process = create_named_pipes_and_pseudo_console(
        COORD { X: cols, Y: rows },
        if conpty_inherit_cursor { 1 } else { 0 },
        pipe_name
      )?;
      // Restore default handling of ctrl+c
      SetConsoleCtrlHandler(None, None);
      Ok(process)
    }
  }
}

/// Start the suspended process previously created with conpty_create_process and contpy_connect.
///
/// Returns the result of the operation.
#[allow(dead_code, unused_variables)]
#[napi(js_name = "conptyStartProcess")]
pub unsafe fn conpty_start_process(
  process: u32
) -> napi::Result<(u32)> {
  #[cfg(not(target_family = "windows"))]
  return err!("Platform not supported");
  #[cfg(target_family = "windows")] {
    let process_handle = process as usize;

    // Get the global handles
    let mut handles = PTY_HANDLES.lock().unwrap();
    // Fetch pty handle from ID and start process
    match handles.get_mut(&process_handle) {
      Some(baton) => {
        // Start process
        let result = ResumeThread( baton.h_thread.unwrap() );
        if result == 0xFFFFFFFF {
          let error_code = GetLastError().0;
          return err!(format!("resume: failed, error code: {}", error_code));
        }
        Ok(result)
      }
      _ => {
        err!("resume: failed, process not found")
      }
    }
  }
}

/// Executes the creation of the shell in the pseudoconsole create previously with conpty_create_process.
/// The process is created in a suspended state to allow for the start of the worker sockets.
///
/// The connection object containing the required values of process id and thread id.
#[allow(unused_variables)]
#[allow(dead_code)]
#[napi]
fn conpty_connect(pty_id: i32, cmdline: String, cwd: String, env: HashMap<String, String>, onexit: JsFunction) -> napi::Result<IConptyConnection> {
  #[cfg(not(target_family = "windows"))]
  return err!("Platform not supported");
  #[cfg(target_family = "windows")] {
    unsafe {
      let process = pty_connect(
        pty_id as usize,
        cmdline, cwd, env,
        onexit
      )?;

      Ok(IConptyConnection { pid: process.dwProcessId, tid: process.dwThreadId })
    }
  }
}

/// This method executes the resize operation on a valid console handle.
#[allow(dead_code, unused_variables)]
#[napi]
fn conpty_resize(pty_id: i32, cols: i32, rows: i32) -> napi::Result<()>{
  #[cfg(not(target_family = "windows"))]
  return err!("Platform not supported");
  #[cfg(target_family = "windows")] {
    let pty_id = pty_id as usize;
    let cols = coerce_i16(cols)?;
    let rows = coerce_i16(rows)?;

    match PTY_HANDLES.lock().expect("Failed to get handle for pty list").get_mut(&pty_id) {
      Some(handle) => {
        let size: COORD = COORD { X: cols, Y: rows };
        unsafe {
          match ResizePseudoConsole(handle.hpc, size) {
            Err(_) => err!("Failed to resize PseudoConsole"),
            Ok(_) => Ok(())
          }
        }
      },
      None => { err!("No pty was found with the provided id") }
    }
  }
}

/// This method executes the kill operation on a valid console handle.
/// Closing the pseudoconsole and the input/output handles attached.
#[allow(dead_code, unused_variables)]
#[napi]
unsafe fn conpty_kill(pty_id: i32) -> napi::Result<()> {
  #[cfg(not(target_family = "windows"))]
  return err!("Platform not supported");
  #[cfg(target_family = "windows")] {
    let pty_id = pty_id as usize;
    match PTY_HANDLES.lock().expect("Failed to get handle for pty list").get_mut(&pty_id) {
      Some(handle) => {
        ClosePseudoConsole(handle.hpc);
        DisconnectNamedPipe(handle.h_in);
        DisconnectNamedPipe(handle.h_out);
        CloseHandle(handle.h_in);
        CloseHandle(handle.h_out);
        CloseHandle(handle.h_shell);
        Ok(())
      },
      None => { err!("No pty was found with the provided id") }
    }
  }
}