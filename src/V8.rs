// Copyright 2019-2020 the Deno authors. All rights reserved. MIT license.
use libc::c_char;
use libc::c_int;
use std::ffi::CStr;
use std::ffi::CString;
use std::sync::Mutex;
use std::vec::Vec;

use crate::platform::Platform;
use crate::support::UniqueRef;
use crate::support::UnitType;

extern "C" {
  fn v8__V8__SetFlagsFromCommandLine(argc: *mut c_int, argv: *mut *mut c_char);
  fn v8__V8__SetFlagsFromString(flags: *const u8, length: usize);
  fn v8__V8__SetEntropySource(callback: EntropySource);
  fn v8__V8__GetVersion() -> *const c_char;
  fn v8__V8__InitializePlatform(platform: *mut Platform);
  fn v8__V8__Initialize();
  fn v8__V8__Dispose() -> bool;
  fn v8__V8__ShutdownPlatform();
}

/// EntropySource is used as a callback function when v8 needs a source
/// of entropy.
#[derive(Copy, Clone)]
#[repr(transparent)]
pub struct EntropySource(RawEntropySource);

pub trait IntoEntropySource:
  UnitType + Into<EntropySource> + FnOnce(&mut [u8]) -> bool
{
}

impl<F> IntoEntropySource for F where
  F: UnitType + Into<EntropySource> + FnOnce(&mut [u8]) -> bool
{
}

type RawEntropySource = extern "C" fn(*mut u8, usize) -> bool;

impl<F> From<F> for EntropySource
where
  F: UnitType + FnOnce(&mut [u8]) -> bool,
{
  fn from(_: F) -> Self {
    #[inline(always)]
    extern "C" fn adapter<F: IntoEntropySource>(
      buffer: *mut u8,
      length: usize,
    ) -> bool {
      let buffer = unsafe { std::slice::from_raw_parts_mut(buffer, length) };
      (F::get())(buffer)
    }

    Self(adapter::<F>)
  }
}

#[derive(Debug, Eq, PartialEq)]
enum GlobalState {
  Uninitialized,
  PlatformInitialized,
  Initialized,
  Disposed,
  PlatformShutdown,
}
use GlobalState::*;

lazy_static! {
  static ref GLOBAL_STATE: Mutex<GlobalState> =
    Mutex::new(GlobalState::Uninitialized);
}

pub fn assert_initialized() {
  let global_state_guard = GLOBAL_STATE.lock().unwrap();
  assert_eq!(*global_state_guard, Initialized);
}

/// Pass the command line arguments to v8.
/// The first element of args (which usually corresponds to the binary name) is
/// ignored.
/// Returns a vector of command line arguments that V8 did not understand.
/// TODO: Check whether this is safe to do after globally initializing v8.
pub fn set_flags_from_command_line(args: Vec<String>) -> Vec<String> {
  // deno_set_v8_flags(int* argc, char** argv) mutates argc and argv to remove
  // flags that v8 understands.

  // Make a new array, that can be modified by V8::SetFlagsFromCommandLine(),
  // containing mutable raw pointers to the individual command line args.
  let mut raw_argv = args
    .iter()
    .map(|arg| CString::new(arg.as_str()).unwrap().into_bytes_with_nul())
    .collect::<Vec<_>>();
  let mut c_argv = raw_argv
    .iter_mut()
    .map(|arg| arg.as_mut_ptr() as *mut c_char)
    .collect::<Vec<_>>();

  // Store the length of the c_argv array in a local variable. We'll pass
  // a pointer to this local variable to deno_set_v8_flags(), which then
  // updates its value.
  let mut c_argv_len = c_argv.len() as c_int;
  // Let v8 parse the arguments it recognizes and remove them from c_argv.
  unsafe {
    v8__V8__SetFlagsFromCommandLine(&mut c_argv_len, c_argv.as_mut_ptr())
  };
  // If c_argv_len was updated we have to change the length of c_argv to match.
  c_argv.truncate(c_argv_len as usize);
  // Copy the modified arguments list into a proper rust vec and return it.
  c_argv
    .iter()
    .map(|ptr| unsafe {
      let cstr = CStr::from_ptr(*ptr as *const c_char);
      let slice = cstr.to_str().unwrap();
      slice.to_string()
    })
    .collect()
}

/// Sets V8 flags from a string.
pub fn set_flags_from_string(flags: &str) {
  unsafe {
    v8__V8__SetFlagsFromString(flags.as_ptr(), flags.len());
  }
}

/// Allows the host application to provide a callback which can be used
/// as a source of entropy for random number generators.
pub fn set_entropy_source(
  callback: impl UnitType + Into<EntropySource> + FnOnce(&mut [u8]) -> bool,
) {
  unsafe { v8__V8__SetEntropySource(callback.into()) };
}

/// Get the version string.
pub fn get_version() -> &'static str {
  let version = unsafe { v8__V8__GetVersion() };
  let c_str = unsafe { CStr::from_ptr(version) };
  c_str.to_str().unwrap()
}

// TODO: V8::InitializePlatform does not actually take a UniquePtr but rather
// a raw pointer. This means that the Platform object is not released when
// V8::ShutdownPlatform is called.
/// Sets the v8::Platform to use. This should be invoked before V8 is
/// initialized.
pub fn initialize_platform(platform: UniqueRef<Platform>) {
  let mut global_state_guard = GLOBAL_STATE.lock().unwrap();
  assert_eq!(*global_state_guard, Uninitialized);
  unsafe { v8__V8__InitializePlatform(platform.into_raw()) };
  *global_state_guard = PlatformInitialized;
}

/// Initializes V8. This function needs to be called before the first Isolate
/// is created. It always returns true.
pub fn initialize() {
  let mut global_state_guard = GLOBAL_STATE.lock().unwrap();
  assert_eq!(*global_state_guard, PlatformInitialized);
  unsafe { v8__V8__Initialize() };
  *global_state_guard = Initialized;
}

/// Releases any resources used by v8 and stops any utility threads
/// that may be running.  Note that disposing v8 is permanent, it
/// cannot be reinitialized.
///
/// It should generally not be necessary to dispose v8 before exiting
/// a process, this should happen automatically.  It is only necessary
/// to use if the process needs the resources taken up by v8.
///
/// # Safety
///
/// Calling this function before completely disposing all isolates will lead
/// to a crash.
pub unsafe fn dispose() -> bool {
  let mut global_state_guard = GLOBAL_STATE.lock().unwrap();
  assert_eq!(*global_state_guard, Initialized);
  assert_eq!(v8__V8__Dispose(), true);
  *global_state_guard = Disposed;
  true
}

/// Clears all references to the v8::Platform. This should be invoked after
/// V8 was disposed.
pub fn shutdown_platform() {
  let mut global_state_guard = GLOBAL_STATE.lock().unwrap();
  assert_eq!(*global_state_guard, Disposed);
  unsafe { v8__V8__ShutdownPlatform() };
  *global_state_guard = PlatformShutdown;
}
