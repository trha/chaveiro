//! Test hook: switches this process, and only this process, to the Vietnamese
//! Telex IME through TSF. The user's language list and other apps are not
//! touched.

use windows_core::{GUID, HRESULT, Result};

windows_core::link!("ole32.dll" "system" fn CoCreateInstance(class: *const GUID, outer: *mut core::ffi::c_void, context: u32, interface: *const GUID, object: *mut *mut core::ffi::c_void) -> HRESULT);

const CLSID_TF_INPUT_PROCESSOR_PROFILES: GUID =
    GUID::from_u128(0x33c53a50_f456_4884_b049_85fd643ecfed);
const IID_ITF_INPUT_PROCESSOR_PROFILE_MGR: GUID =
    GUID::from_u128(0x71c6e74c_0f28_11d8_a82a_00065b84435c);
const CLSCTX_INPROC_SERVER: u32 = 1;
const TF_PROFILETYPE_INPUTPROCESSOR: u32 = 1;
const TF_IPPMF_FORPROCESS: u32 = 0x1000_0000;
/// Vietnamese Telex, as listed by `Get-WinUserLanguageList` on this machine.
const VIETNAMESE: u16 = 0x042a;
const TELEX_CLSID: GUID = GUID::from_u128(0xc2cb2cf0_af47_413e_9780_8bc3a3c16068);
const TELEX_PROFILE: GUID = GUID::from_u128(0x5fb02ec5_0a77_4684_b4fa_def8a2195628);

/// `ITfInputProcessorProfileMgr`: IUnknown's three slots, then `ActivateProfile`.
#[repr(C)]
struct ProfileManagerVtable {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32,
    activate_profile: unsafe extern "system" fn(
        this: *mut core::ffi::c_void,
        profile_type: u32,
        language: u16,
        class: *const GUID,
        profile: *const GUID,
        keyboard_layout: *mut core::ffi::c_void,
        flags: u32,
    ) -> HRESULT,
}

/// Activates Vietnamese Telex for this process. Call on the UI thread after
/// COM is initialized.
pub fn activate_vietnamese_telex() -> Result<()> {
    unsafe {
        let mut manager = core::ptr::null_mut();
        CoCreateInstance(
            &CLSID_TF_INPUT_PROCESSOR_PROFILES,
            core::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_ITF_INPUT_PROCESSOR_PROFILE_MGR,
            &mut manager,
        )
        .ok()?;
        let vtable = &**(manager as *mut *const ProfileManagerVtable);
        let activated = (vtable.activate_profile)(
            manager,
            TF_PROFILETYPE_INPUTPROCESSOR,
            VIETNAMESE,
            &TELEX_CLSID,
            &TELEX_PROFILE,
            core::ptr::null_mut(),
            TF_IPPMF_FORPROCESS,
        );
        (vtable.release)(manager);
        activated.ok()
    }
}
