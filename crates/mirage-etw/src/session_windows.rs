use std::mem::size_of;
use std::ptr;
use std::sync::Mutex;
use std::thread::JoinHandle;

use mirage_types::MirageError;
use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_CANCELLED, ERROR_SUCCESS, ERROR_WMI_INSTANCE_NOT_FOUND,
    GetLastError,
};
use windows_sys::Win32::System::Diagnostics::Etw::{
    CONTROLTRACE_HANDLE, CloseTrace, ControlTraceW, EVENT_RECORD, EVENT_TRACE_CONTROL_STOP,
    EVENT_TRACE_FLAG_DISK_FILE_IO, EVENT_TRACE_FLAG_DISK_IO, EVENT_TRACE_FLAG_FILE_IO,
    EVENT_TRACE_FLAG_FILE_IO_INIT, EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES,
    EVENT_TRACE_REAL_TIME_MODE, EVENT_TRACE_SYSTEM_LOGGER_MODE, FileIoGuid, OpenTraceW,
    PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_RAW_TIMESTAMP,
    PROCESS_TRACE_MODE_REAL_TIME, PROCESSTRACE_HANDLE, PROPERTY_DATA_DESCRIPTOR, ProcessTrace,
    StartTraceW, TdhGetProperty, TdhGetPropertySize, WNODE_FLAG_TRACED_GUID,
};
use windows_sys::Win32::System::Performance::QueryPerformanceFrequency;

use crate::correlate::{Correlator, TraceEvent};
use crate::session::{CapturedSession, SessionMetrics};

const INVALID_PROCESSTRACE_HANDLE: u64 = u64::MAX;
const FILE_IO_NAME: u8 = 0;
const FILE_IO_FILE_CREATE: u8 = 32;
const FILE_IO_FILE_DELETE: u8 = 35;
const FILE_IO_FILE_RUNDOWN: u8 = 36;
const FILE_IO_CREATE: u8 = 64;
const FILE_IO_CLEANUP: u8 = 65;
const FILE_IO_CLOSE: u8 = 66;
const FILE_IO_READ: u8 = 67;
const FILE_IO_WRITE: u8 = 68;
const MAX_PROPERTY_BYTES: u32 = 64 * 1024;

#[derive(Debug)]
struct CaptureData {
    correlator: Correlator,
    events: Vec<TraceEvent>,
    maximum_events: usize,
    unknown_paths: u64,
    dropped_events: u64,
}

impl CaptureData {
    fn new(maximum_events: usize) -> Self {
        Self {
            correlator: Correlator::new(maximum_events.clamp(1, 1_000_000)),
            events: Vec::with_capacity(maximum_events.min(65_536)),
            maximum_events,
            unknown_paths: 0,
            dropped_events: 0,
        }
    }
}

#[derive(Debug)]
struct CaptureContext {
    qpc_frequency: u64,
    data: Mutex<CaptureData>,
}

pub struct EtwSession {
    handle: CONTROLTRACE_HANDLE,
    storage: Vec<u64>,
    consumer_handle: PROCESSTRACE_HANDLE,
    consumer_thread: Option<JoinHandle<u32>>,
    capture_context: *mut CaptureContext,
    stopped: bool,
}

impl EtwSession {
    pub fn start(
        name: &str,
        minimum_buffers: u32,
        maximum_buffers: u32,
    ) -> Result<Self, MirageError> {
        Self::start_inner(name, minimum_buffers, maximum_buffers, 0)
    }

    pub fn start_capture(
        name: &str,
        minimum_buffers: u32,
        maximum_buffers: u32,
        maximum_events: usize,
    ) -> Result<Self, MirageError> {
        if maximum_events == 0 || maximum_events > 10_000_000 {
            return Err(MirageError::invalid_argument(
                "ETW capture event bound is outside supported limits",
            ));
        }
        Self::start_inner(name, minimum_buffers, maximum_buffers, maximum_events)
    }

    fn start_inner(
        name: &str,
        minimum_buffers: u32,
        maximum_buffers: u32,
        maximum_events: usize,
    ) -> Result<Self, MirageError> {
        if name.is_empty()
            || name.len() > 256
            || name.chars().any(char::is_control)
            || minimum_buffers < 2
            || maximum_buffers < minimum_buffers
            || maximum_buffers > 1024
        {
            return Err(MirageError::invalid_argument(
                "invalid ETW session configuration",
            ));
        }
        let name_wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let bytes = size_of::<EVENT_TRACE_PROPERTIES>() + name_wide.len() * size_of::<u16>();
        let mut storage = vec![0_u64; bytes.div_ceil(size_of::<u64>())];
        let properties = storage.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
        // SAFETY: storage is aligned and sized for properties plus the UTF-16 name and remains
        // owned until the trace is stopped.
        unsafe {
            ptr::write(properties, EVENT_TRACE_PROPERTIES::default());
            (*properties).Wnode.BufferSize = bytes as u32;
            (*properties).Wnode.ClientContext = 1;
            (*properties).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
            (*properties).BufferSize = 64;
            (*properties).MinimumBuffers = minimum_buffers;
            (*properties).MaximumBuffers = maximum_buffers;
            (*properties).FlushTimer = 1;
            (*properties).LogFileMode = EVENT_TRACE_REAL_TIME_MODE | EVENT_TRACE_SYSTEM_LOGGER_MODE;
            // Microsoft documents DISK_FILE_IO (with DISK_IO) as the switch that emits the
            // FileIo_Name events needed to correlate read/write FileObject values to paths.
            (*properties).EnableFlags = EVENT_TRACE_FLAG_DISK_IO
                | EVENT_TRACE_FLAG_DISK_FILE_IO
                | EVENT_TRACE_FLAG_FILE_IO
                | EVENT_TRACE_FLAG_FILE_IO_INIT;
            (*properties).LoggerNameOffset = size_of::<EVENT_TRACE_PROPERTIES>() as u32;
            ptr::copy_nonoverlapping(
                name_wide.as_ptr(),
                properties
                    .cast::<u8>()
                    .add(size_of::<EVENT_TRACE_PROPERTIES>())
                    .cast::<u16>(),
                name_wide.len(),
            );
        }
        let mut handle = CONTROLTRACE_HANDLE::default();
        // SAFETY: name is terminated, properties points to a stable initialized allocation, and
        // handle is writable.
        let status = unsafe { StartTraceW(&mut handle, name_wide.as_ptr(), properties) };
        if status != ERROR_SUCCESS {
            let message = if status == ERROR_ALREADY_EXISTS {
                format!("ETW session name already exists (Windows error {status})")
            } else {
                format!(
                    "cannot start ETW file-I/O session (Windows error {status}); elevation may be required"
                )
            };
            return Err(MirageError::provider_unavailable(message));
        }

        let mut qpc_frequency = 0_i64;
        // SAFETY: qpc_frequency points to writable storage.
        if unsafe { QueryPerformanceFrequency(&mut qpc_frequency) } == 0 || qpc_frequency <= 0 {
            // SAFETY: the trace was just started and the properties allocation is still valid.
            unsafe {
                ControlTraceW(handle, ptr::null(), properties, EVENT_TRACE_CONTROL_STOP);
            }
            return Err(MirageError::provider_unavailable(
                "cannot query the ETW timestamp frequency",
            ));
        }

        let capture_context = Box::into_raw(Box::new(CaptureContext {
            qpc_frequency: qpc_frequency as u64,
            data: Mutex::new(CaptureData::new(maximum_events)),
        }));
        let mut logfile = EVENT_TRACE_LOGFILEW {
            LoggerName: name_wide.as_ptr().cast_mut(),
            Context: capture_context.cast(),
            ..EVENT_TRACE_LOGFILEW::default()
        };
        logfile.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME
            | PROCESS_TRACE_MODE_EVENT_RECORD
            | PROCESS_TRACE_MODE_RAW_TIMESTAMP;
        logfile.Anonymous2.EventRecordCallback = Some(capture_event);
        // SAFETY: logfile is fully initialized for a real-time consumer. The context allocation
        // remains live until ProcessTrace has returned.
        let consumer_handle = unsafe { OpenTraceW(&mut logfile) };
        if consumer_handle.Value == INVALID_PROCESSTRACE_HANDLE {
            let windows_error = unsafe { GetLastError() };
            // SAFETY: both handles and allocations were created above and are released exactly once.
            unsafe {
                ControlTraceW(handle, ptr::null(), properties, EVENT_TRACE_CONTROL_STOP);
                drop(Box::from_raw(capture_context));
            }
            return Err(MirageError::provider_unavailable(format!(
                "cannot open ETW real-time consumer (Windows error {windows_error})",
            )));
        }
        let consumer_thread = match std::thread::Builder::new()
            .name(format!("mirage-etw-{name}"))
            .spawn(move || {
                // SAFETY: consumer_handle is valid and remains owned until processing returns.
                unsafe { ProcessTrace(&consumer_handle, 1, ptr::null(), ptr::null()) }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                // SAFETY: all resources were successfully created and are still exclusively owned.
                unsafe {
                    ControlTraceW(handle, ptr::null(), properties, EVENT_TRACE_CONTROL_STOP);
                    CloseTrace(consumer_handle);
                    drop(Box::from_raw(capture_context));
                }
                return Err(MirageError::from(error));
            }
        };
        Ok(Self {
            handle,
            storage,
            consumer_handle,
            consumer_thread: Some(consumer_thread),
            capture_context,
            stopped: false,
        })
    }

    pub fn stop(mut self) -> Result<SessionMetrics, MirageError> {
        self.stop_inner().map(|capture| capture.metrics)
    }

    pub fn stop_capture(mut self) -> Result<CapturedSession, MirageError> {
        self.stop_inner()
    }

    fn stop_inner(&mut self) -> Result<CapturedSession, MirageError> {
        if self.capture_context.is_null() {
            return Err(MirageError::internal_invariant(
                "ETW session was already consumed",
            ));
        }
        let mut cleanup_error = None;
        if !self.stopped {
            let properties = self.storage.as_mut_ptr().cast::<EVENT_TRACE_PROPERTIES>();
            // SAFETY: handle came from StartTraceW and properties retains its allocation.
            let status = unsafe {
                ControlTraceW(
                    self.handle,
                    ptr::null(),
                    properties,
                    EVENT_TRACE_CONTROL_STOP,
                )
            };
            self.stopped = true;
            if status != ERROR_SUCCESS && status != ERROR_WMI_INSTANCE_NOT_FOUND {
                cleanup_error = Some(MirageError::provider_unavailable(
                    "cannot stop ETW file-I/O session",
                ));
            }
        }

        let processing_status = if let Some(thread) = self.consumer_thread.take() {
            match thread.join() {
                Ok(status) => status,
                Err(_) => {
                    cleanup_error.get_or_insert_with(|| {
                        MirageError::internal_invariant("ETW consumer thread panicked")
                    });
                    ERROR_CANCELLED
                }
            }
        } else {
            ERROR_SUCCESS
        };
        // SAFETY: the processing thread has returned, so the consumer handle is no longer in use.
        let close_status = unsafe { CloseTrace(self.consumer_handle) };
        if processing_status != ERROR_SUCCESS
            && processing_status != ERROR_CANCELLED
            && processing_status != ERROR_WMI_INSTANCE_NOT_FOUND
        {
            cleanup_error.get_or_insert_with(|| {
                MirageError::provider_unavailable(format!(
                    "ETW real-time consumer failed with Windows error {processing_status}",
                ))
            });
        }
        if close_status != ERROR_SUCCESS && close_status != ERROR_WMI_INSTANCE_NOT_FOUND {
            cleanup_error.get_or_insert_with(|| {
                MirageError::provider_unavailable("cannot close ETW real-time consumer")
            });
        }

        let metrics = self.metrics();
        // SAFETY: ProcessTrace has returned and no callback can access the allocation. Clearing the
        // field prevents Drop from reclaiming it a second time.
        let context = unsafe { Box::from_raw(self.capture_context) };
        self.capture_context = ptr::null_mut();
        let mut data = context
            .data
            .into_inner()
            .map_err(|_| MirageError::internal_invariant("ETW capture state was poisoned"))?;
        data.events.sort_by_key(|event| event.timestamp_100ns);
        let capture = CapturedSession {
            metrics,
            events: data.events,
            unknown_paths: data.unknown_paths,
            dropped_events: data.dropped_events,
        };
        if let Some(error) = cleanup_error {
            Err(error)
        } else {
            Ok(capture)
        }
    }

    fn metrics(&self) -> SessionMetrics {
        let properties = self.storage.as_ptr().cast::<EVENT_TRACE_PROPERTIES>();
        // SAFETY: storage contains an initialized EVENT_TRACE_PROPERTIES value.
        unsafe {
            SessionMetrics {
                events_lost: (*properties).EventsLost,
                realtime_buffers_lost: (*properties).RealTimeBuffersLost,
                buffers_written: (*properties).BuffersWritten,
            }
        }
    }
}

impl std::fmt::Debug for EtwSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EtwSession")
            .field("handle", &self.handle.Value)
            .field("consumer_handle", &self.consumer_handle.Value)
            .field("stopped", &self.stopped)
            .finish()
    }
}

impl Drop for EtwSession {
    fn drop(&mut self) {
        if !self.capture_context.is_null() {
            let _ = self.stop_inner();
        }
    }
}

unsafe extern "system" fn capture_event(record: *mut EVENT_RECORD) {
    if record.is_null() {
        return;
    }
    // SAFETY: ETW supplies a valid EVENT_RECORD for the duration of this callback.
    let event = unsafe { &*record };
    if !same_guid(&event.EventHeader.ProviderId, &FileIoGuid) || event.UserContext.is_null() {
        return;
    }
    // SAFETY: UserContext was supplied by start_inner and remains live until ProcessTrace returns.
    let context = unsafe { &*(event.UserContext.cast::<CaptureContext>()) };
    let Ok(mut data) = context.data.lock() else {
        return;
    };
    let opcode = event.EventHeader.EventDescriptor.Opcode;
    match opcode {
        FILE_IO_NAME | FILE_IO_FILE_CREATE | FILE_IO_FILE_DELETE | FILE_IO_FILE_RUNDOWN => {
            let Some(file_key) = event_file_key(event) else {
                return;
            };
            let Some(path) = property_utf16(event, "FileName") else {
                return;
            };
            data.correlator.name(file_key, path.into());
        }
        FILE_IO_CREATE => {
            let Some(file_key) = event_file_key(event) else {
                return;
            };
            let Some(path) = property_utf16(event, "OpenPath") else {
                return;
            };
            data.correlator.name(file_key, path.into());
        }
        FILE_IO_CLEANUP | FILE_IO_CLOSE => {
            if let Some(file_key) = event_file_key(event) {
                data.correlator.remove(file_key);
            }
        }
        FILE_IO_READ | FILE_IO_WRITE => {
            let (Some(file_key), Some(offset), Some(size)) = (
                event_file_key(event),
                property_u64(event, "Offset"),
                property_u32(event, "IoSize"),
            ) else {
                data.dropped_events = data.dropped_events.saturating_add(1);
                return;
            };
            let timestamp = event.EventHeader.TimeStamp.max(0) as u64;
            let timestamp_100ns = ((timestamp as u128).saturating_mul(10_000_000)
                / context.qpc_frequency as u128)
                .min(u64::MAX as u128) as u64;
            let trace_event = data.correlator.io(
                timestamp_100ns,
                event.EventHeader.ProcessId,
                file_key,
                offset,
                size,
                opcode == FILE_IO_WRITE,
            );
            if trace_event.path.is_none() {
                data.unknown_paths = data.unknown_paths.saturating_add(1);
            }
            if data.events.len() >= data.maximum_events {
                data.dropped_events = data.dropped_events.saturating_add(1);
            } else if data.maximum_events != 0 {
                data.events.push(trace_event);
            }
        }
        _ => {}
    }
}

fn event_file_key(event: &EVENT_RECORD) -> Option<u64> {
    property_u64(event, "FileObject").or_else(|| property_u64(event, "FileKey"))
}

fn property_u32(event: &EVENT_RECORD, name: &str) -> Option<u32> {
    let bytes = property_bytes(event, name)?;
    match bytes.len() {
        4.. => Some(u32::from_le_bytes(bytes[..4].try_into().ok()?)),
        _ => None,
    }
}

fn property_u64(event: &EVENT_RECORD, name: &str) -> Option<u64> {
    let bytes = property_bytes(event, name)?;
    match bytes.len() {
        8.. => Some(u64::from_le_bytes(bytes[..8].try_into().ok()?)),
        4.. => Some(u32::from_le_bytes(bytes[..4].try_into().ok()?) as u64),
        _ => None,
    }
}

fn property_utf16(event: &EVENT_RECORD, name: &str) -> Option<String> {
    let bytes = property_bytes(event, name)?;
    if bytes.len() < 2 || bytes.len() % 2 != 0 {
        return None;
    }
    let mut units = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    if units.last() == Some(&0) {
        units.pop();
    }
    String::from_utf16(&units)
        .ok()
        .filter(|value| !value.is_empty())
}

fn property_bytes(event: &EVENT_RECORD, name: &str) -> Option<Vec<u8>> {
    let wide = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let descriptor = PROPERTY_DATA_DESCRIPTOR {
        PropertyName: wide.as_ptr() as u64,
        ArrayIndex: u32::MAX,
        Reserved: 0,
    };
    let mut size = 0_u32;
    // SAFETY: event is valid during the callback and descriptor references a terminated name.
    let status = unsafe { TdhGetPropertySize(event, 0, ptr::null(), 1, &descriptor, &mut size) };
    if status != ERROR_SUCCESS || size == 0 || size > MAX_PROPERTY_BYTES {
        return None;
    }
    let mut bytes = vec![0_u8; size as usize];
    // SAFETY: the destination is exactly the size TDH requested for the same event/property.
    let status = unsafe {
        TdhGetProperty(
            event,
            0,
            ptr::null(),
            1,
            &descriptor,
            size,
            bytes.as_mut_ptr(),
        )
    };
    (status == ERROR_SUCCESS).then_some(bytes)
}

fn same_guid(left: &windows_sys::core::GUID, right: &windows_sys::core::GUID) -> bool {
    left.data1 == right.data1
        && left.data2 == right.data2
        && left.data3 == right.data3
        && left.data4 == right.data4
}
