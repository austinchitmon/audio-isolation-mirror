//! Per-app Windows audio output routing.
//!
//! Windows has no public/documented API for assigning a specific running
//! app's audio output to a specific device (the "App volume and device
//! preferences" feature in Settings > Sound). This module talks directly to
//! the same undocumented mechanism Windows' own Volume Mixer uses
//! internally: a WinRT runtime class, `Windows.Media.Internal.AudioPolicyConfig`,
//! implemented in `AudioSes.dll` and activated via `RoGetActivationFactory`
//! rather than the classic `CoCreateInstance`/CLSID path (that classic path
//! -- `IPolicyConfig` via `CLSID_PolicyConfigClient` -- still exists but only
//! covers the *global* default device, not per-app routing).
//!
//! Reverse-engineered by the open-source EarTrumpet
//! (https://github.com/File-New-Project/EarTrumpet) and SoundSwitch
//! (https://github.com/Belphemur/SoundSwitch) projects; this module's
//! activation sequence and vtable layout are ported from SoundSwitch's
//! `AudioPolicyConfig.cs`, which is actively maintained against current
//! Windows 11 builds. There is no warranty on any of this -- it could change
//! or disappear in a future Windows update -- so every caller must be
//! prepared to fall back to the manual Volume Mixer method (see README).

use std::ffi::c_void;
use std::sync::Once;

use anyhow::{anyhow, Context, Result};
use windows::core::{Interface, GUID, HRESULT, HSTRING, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, MAX_PATH};
use windows::Win32::Media::Audio::{
    eConsole, eMultimedia, eRender, EDataFlow, ERole, IAudioSessionControl2, IAudioSessionManager2,
    IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::WinRT::RoGetActivationFactory;

/// Ensures COM is initialized on the calling (UI) thread. Safe to call
/// repeatedly; never uninitializes, since this is a long-lived GUI thread
/// and winit/eframe likely already initialized COM for OS drag-and-drop.
fn ensure_com_initialized() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        // S_OK (we own it), S_FALSE (already initialized), and
        // RPC_E_CHANGED_MODE (already initialized under a different
        // concurrency model but still usable from this thread) are all
        // fine to proceed with.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    });
}

/// A running process with an active (currently playing) audio render session.
pub struct AudioSession {
    pub pid: u32,
    pub exe_name: String,
}

fn exe_name_for_pid(pid: u32) -> Option<String> {
    unsafe {
        let handle: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; MAX_PATH as usize];
        let mut len = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(handle);
        result.ok()?;

        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().map(|s| s.to_string())
    }
}

/// Enumerates every currently-active audio render (playback) session across
/// every active render endpoint, i.e. every process that is actively making
/// sound right now. Deduplicated by pid.
pub fn list_active_render_sessions() -> Result<Vec<AudioSession>> {
    ensure_com_initialized();
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .context("failed to create MMDeviceEnumerator")?;

        let endpoints = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .context("failed to enumerate render endpoints")?;
        let count = endpoints.GetCount().context("failed to get endpoint count")?;

        let mut pids = Vec::new();
        for i in 0..count {
            let Ok(device) = endpoints.Item(i) else { continue };
            let Ok(session_manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None)
            else {
                continue;
            };
            let Ok(session_enum) = session_manager.GetSessionEnumerator() else { continue };
            let Ok(session_count) = session_enum.GetCount() else { continue };

            for j in 0..session_count {
                let Ok(session) = session_enum.GetSession(j) else { continue };
                let Ok(session2) = session.cast::<IAudioSessionControl2>() else { continue };

                // IsSystemSoundsSession returns a raw HRESULT, not a windows-rs
                // Result: S_OK means "yes, this is the system sounds session" and
                // S_FALSE means "no" -- both are non-error HRESULTs, so `.is_ok()`
                // (which is true for S_FALSE too) would wrongly skip every session.
                if session2.IsSystemSoundsSession() == windows::Win32::Foundation::S_OK {
                    continue;
                }
                let Ok(pid) = session2.GetProcessId() else { continue };
                if pid != 0 && !pids.contains(&pid) {
                    pids.push(pid);
                }
            }
        }

        Ok(pids
            .into_iter()
            .map(|pid| AudioSession {
                pid,
                exe_name: exe_name_for_pid(pid).unwrap_or_else(|| format!("<pid {pid}>")),
            })
            .collect())
    }
}

/// Finds a currently-active render (playback) endpoint whose friendly name
/// contains `needle` (case-insensitive), returning its raw WASAPI device ID
/// string. Used to locate "CABLE Input (VB-Audio Virtual Cable)".
pub fn find_render_endpoint_id_by_name(needle: &str) -> Result<Option<String>> {
    ensure_com_initialized();
    let needle = needle.to_lowercase();
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .context("failed to create MMDeviceEnumerator")?;

        let endpoints = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .context("failed to enumerate render endpoints")?;
        let count = endpoints.GetCount().context("failed to get endpoint count")?;

        for i in 0..count {
            let Ok(device) = endpoints.Item(i) else { continue };
            let Ok(store) = device.OpenPropertyStore(STGM_READ) else { continue };
            let Ok(name_prop) = store.GetValue(&PKEY_Device_FriendlyName) else { continue };
            let name = name_prop.to_string();

            if name.to_lowercase().contains(&needle) {
                let id = device.GetId().context("failed to get device id")?;
                let id = id.to_string().context("device id was not valid UTF-16")?;
                return Ok(Some(id));
            }
        }
        Ok(None)
    }
}

/// IIDs of the extended per-app audio policy interface exposed by the
/// `Windows.Media.Internal.AudioPolicyConfig` WinRT class's activation
/// factory, keyed by which Windows build implements them. Different builds
/// implement different (incompatible) versions of this undocumented
/// interface, so callers must try each in turn. Source: SoundSwitch's
/// `AudioPolicyConfig.cs` `_knownValidGuids`.
const KNOWN_AUDIO_POLICY_CONFIG_IIDS: [GUID; 3] = [
    GUID::from_u128(0xab3d4648_e242_459f_b02f_541c70306324), // Windows 11 / 20H1+
    GUID::from_u128(0x2a59116d_6c4f_45e0_a74f_707e3fef9258), // pre-20H1 Windows 10
    GUID::from_u128(0x32aa8e18_6496_4e24_9f94_b800e7eccc45), // Windows 10 1709 (RS3)
];

/// Vtable slots (0-indexed, including the 6 IUnknown/IInspectable base
/// slots) of `SetPersistedDefaultAudioEndpoint`/`GetPersistedDefaultAudioEndpoint`
/// on the interfaces above. The interface also carries the ~19 methods of
/// the classic (global-only) `IPolicyConfig` before these, which is why the
/// offsets are high; see SoundSwitch's `AudioPolicyConfig.cs`
/// (`_vfTable + ptrSize * 25` / `* 26`).
const SLOT_SET_PERSISTED_DEFAULT_AUDIO_ENDPOINT: usize = 25;
const SLOT_GET_PERSISTED_DEFAULT_AUDIO_ENDPOINT: usize = 26;

type SetPersistedDefaultAudioEndpointFn =
    unsafe extern "system" fn(*mut c_void, u32, EDataFlow, ERole, HSTRING) -> HRESULT;
type GetPersistedDefaultAudioEndpointFn =
    unsafe extern "system" fn(*mut c_void, u32, EDataFlow, ERole, *mut HSTRING) -> HRESULT;

/// A device interface path prefix/suffix wrapping is required around the
/// bare WASAPI endpoint ID before passing it to
/// `SetPersistedDefaultAudioEndpoint` -- the bare ID alone is rejected.
/// Source: SoundSwitch's `ExtendedPolicyClient.GenerateDeviceId`.
const MMDEVAPI_TOKEN: &str = r"\\?\SWD#MMDEVAPI#";
const DEVINTERFACE_AUDIO_RENDER: &str = "#{e6327cad-dcec-4949-ae8a-991e976a79d2}";

/// Activates `Windows.Media.Internal.AudioPolicyConfig` and queries its
/// factory for whichever of `KNOWN_AUDIO_POLICY_CONFIG_IIDS` this Windows
/// build implements.
unsafe fn get_audio_policy_config() -> Result<windows::core::IUnknown> {
    let class_id = HSTRING::from("Windows.Media.Internal.AudioPolicyConfig");
    let factory: windows::core::IUnknown = RoGetActivationFactory(&class_id).context(
        "failed to activate the per-app audio routing interface \
         (may be unavailable on this Windows version)",
    )?;

    for iid in KNOWN_AUDIO_POLICY_CONFIG_IIDS {
        let mut raw: *mut c_void = std::ptr::null_mut();
        let hr = factory.query(&iid, &mut raw);
        if hr.is_ok() && !raw.is_null() {
            return Ok(windows::core::IUnknown::from_raw(raw));
        }
    }
    Err(anyhow!(
        "this Windows version's per-app audio routing interface didn't match any known version"
    ))
}

/// Sets (or, if `wrapped_device_id` is `None`, clears back to "Default")
/// `pid`'s persisted render endpoint override for one role. `wrapped_device_id`
/// must already carry the `MMDEVAPI_TOKEN`/`DEVINTERFACE_AUDIO_RENDER` wrapping.
unsafe fn set_persisted_endpoint(
    config: &windows::core::IUnknown,
    pid: u32,
    role: ERole,
    wrapped_device_id: Option<&str>,
) -> Result<()> {
    let obj_ptr = Interface::as_raw(config);
    let vtable_ptr = *(obj_ptr as *const *const c_void);
    let set_fn_ptr =
        *(vtable_ptr as *const *const c_void).add(SLOT_SET_PERSISTED_DEFAULT_AUDIO_ENDPOINT);
    let set_fn: SetPersistedDefaultAudioEndpointFn = std::mem::transmute(set_fn_ptr);

    // An empty HSTRING clears the override entirely, back to "Default" --
    // verified against this interface directly: Set("") then Get() returns
    // success with an empty string, matching the "no override" state a
    // never-routed process starts in.
    let hstr = wrapped_device_id.map(HSTRING::from).unwrap_or_default();
    let hr = set_fn(obj_ptr, pid, eRender, role, hstr);
    hr.ok()
        .with_context(|| format!("failed to set default endpoint for role {role:?}"))
}

/// Reads `pid`'s current persisted render endpoint override for one role.
/// `None` means no override is set (Windows treats this process as
/// "Default" for this role).
unsafe fn get_persisted_endpoint(
    config: &windows::core::IUnknown,
    pid: u32,
    role: ERole,
) -> Result<Option<String>> {
    let obj_ptr = Interface::as_raw(config);
    let vtable_ptr = *(obj_ptr as *const *const c_void);
    let get_fn_ptr =
        *(vtable_ptr as *const *const c_void).add(SLOT_GET_PERSISTED_DEFAULT_AUDIO_ENDPOINT);
    let get_fn: GetPersistedDefaultAudioEndpointFn = std::mem::transmute(get_fn_ptr);

    let mut out = HSTRING::new();
    let hr = get_fn(obj_ptr, pid, eRender, role, &mut out);
    hr.ok()
        .with_context(|| format!("failed to get default endpoint for role {role:?}"))?;
    let value = out.to_string_lossy();
    Ok(if value.is_empty() { None } else { Some(value) })
}

/// Snapshot of a process's per-role render endpoint overrides, as they were
/// before this app touched them. Captured by `get_endpoint_override` and
/// later handed to `restore_endpoint` to put things back exactly as they
/// were -- whether that's "Default" (no override) or some other device the
/// user had already assigned that app themselves.
#[derive(Clone)]
pub struct EndpointOverride {
    console: Option<String>,
    multimedia: Option<String>,
}

/// Reads `pid`'s current per-app render endpoint overrides. Call this
/// *before* routing, so the result can be passed to `restore_endpoint`
/// afterward.
pub fn get_endpoint_override(pid: u32) -> Result<EndpointOverride> {
    ensure_com_initialized();
    unsafe {
        let config = get_audio_policy_config()?;
        Ok(EndpointOverride {
            console: get_persisted_endpoint(&config, pid, eConsole)?,
            multimedia: get_persisted_endpoint(&config, pid, eMultimedia)?,
        })
    }
}

/// Assigns `pid`'s default render (playback) endpoint to `device_id`
/// (a raw WASAPI device ID string, e.g. from `find_render_endpoint_id_by_name`).
/// Persisted by Windows per app identity, so it survives the app restarting.
pub fn route_process_to_endpoint(pid: u32, device_id: &str) -> Result<()> {
    ensure_com_initialized();
    unsafe {
        let config = get_audio_policy_config()?;
        let wrapped_id = format!("{MMDEVAPI_TOKEN}{device_id}{DEVINTERFACE_AUDIO_RENDER}");
        for role in [eConsole, eMultimedia] {
            set_persisted_endpoint(&config, pid, role, Some(&wrapped_id))?;
        }
        Ok(())
    }
}

/// Restores `pid`'s per-app render endpoint overrides to a previously
/// captured `EndpointOverride`, clearing back to "Default" for any role
/// that didn't have an override before this app touched it.
pub fn restore_endpoint(pid: u32, original: &EndpointOverride) -> Result<()> {
    ensure_com_initialized();
    unsafe {
        let config = get_audio_policy_config()?;
        set_persisted_endpoint(&config, pid, eConsole, original.console.as_deref())?;
        set_persisted_endpoint(&config, pid, eMultimedia, original.multimedia.as_deref())?;
        Ok(())
    }
}

