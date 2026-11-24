//! A Gaussian blur effect description for Composition, so the crate needs
//! no Win2D. Composition reads an effect through `IGraphicsEffect` and the
//! COM interface `IGraphicsEffectD2D1Interop`, which names a Direct2D effect
//! and hands over its properties and sources.

// The interop methods keep the names the C header gives them.
#![allow(non_snake_case)]

use crate::bindings::composition::{
    CompositionEffectSourceParameter, IGraphicsEffect, IGraphicsEffect_Impl, IGraphicsEffectSource,
    IGraphicsEffectSource_Impl, IPropertyValue,
};
use std::{cell::RefCell, ffi::c_void};
use windows_core::{
    Error, GUID, HRESULT, HSTRING, IInspectable, Interface, PCWSTR, Result, implement_decl,
    interface_decl,
};
use windows_reference::IReference;

/// The name a brush's animatable property takes: the effect's name, a dot,
/// and the property's name.
pub(crate) const AMOUNT: &str = "Blur.BlurAmount";
/// The brush's source parameter.
pub(crate) const SOURCE: &str = "source";

/// A blur of the brush source [`SOURCE`], named so that its amount is
/// [`AMOUNT`].
pub(crate) fn effect() -> Result<IGraphicsEffect> {
    Ok(Blur {
        name: RefCell::new(HSTRING::from("Blur")),
        source: CompositionEffectSourceParameter::Create(SOURCE)?.cast()?,
    }
    .into())
}

// Declared by hand from windows.graphics.effects.interop.h, because no
// metadata holds it.
interface_decl! {
    unsafe trait IGraphicsEffectD2D1Interop(IGraphicsEffectD2D1Interop_Vtbl, IGraphicsEffectD2D1Interop_Impl): windows_core::IUnknown
        = 0x2fc57384_a068_44d7_a331_30982fcf7177
    {
        unsafe fn GetEffectId(&self, id: *mut GUID) -> Result<()>;
        unsafe fn GetNamedPropertyMapping(&self, name: PCWSTR, index: *mut u32, mapping: *mut i32) -> Result<()>;
        unsafe fn GetPropertyCount(&self, count: *mut u32) -> Result<()>;
        unsafe fn GetProperty(&self, index: u32, value: *mut *mut c_void) -> Result<()>;
        unsafe fn GetSource(&self, index: u32, source: *mut *mut c_void) -> Result<()>;
        unsafe fn GetSourceCount(&self, count: *mut u32) -> Result<()>;
    }
}

/// `CLSID_D2D1GaussianBlur`.
const GAUSSIAN_BLUR: GUID = GUID::from_u128(0x1feb6d69_2fe6_4ac9_8c58_1d7f93e7a6a5);
/// `GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT`: the value reaches Direct2D as
/// it is.
const MAPPING_DIRECT: i32 = 1;
/// The names of `D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION` (f32),
/// `_OPTIMIZATION` (u32) and `_BORDER_MODE` (u32), by index.
const PROPERTIES: [&str; 3] = ["BlurAmount", "Optimization", "BorderMode"];
const E_INVALIDARG: HRESULT = HRESULT(0x8007_0057_u32 as i32);
const E_BOUNDS: HRESULT = HRESULT(0x8000_000B_u32 as i32);

struct Blur {
    name: RefCell<HSTRING>,
    source: IGraphicsEffectSource,
}

implement_decl! {
    impl Blur as Blur_Impl: [IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop]
}

impl IGraphicsEffectSource_Impl for Blur_Impl {}

impl IGraphicsEffect_Impl for Blur_Impl {
    fn Name(&self) -> Result<HSTRING> {
        Ok(self.name.borrow().clone())
    }

    fn SetName(&self, value: &HSTRING) -> Result<()> {
        *self.name.borrow_mut() = value.clone();
        Ok(())
    }
}

impl IGraphicsEffectD2D1Interop_Impl for Blur_Impl {
    unsafe fn GetEffectId(&self, id: *mut GUID) -> Result<()> {
        // SAFETY: Composition passes a valid out pointer.
        unsafe { *id = GAUSSIAN_BLUR };
        Ok(())
    }

    unsafe fn GetNamedPropertyMapping(
        &self,
        name: PCWSTR,
        index: *mut u32,
        mapping: *mut i32,
    ) -> Result<()> {
        // SAFETY: Composition passes a NUL-terminated name.
        let name = unsafe { name.to_string() }.map_err(|_| Error::from(E_INVALIDARG))?;
        let position = PROPERTIES
            .iter()
            .position(|known| known.eq_ignore_ascii_case(&name))
            .ok_or_else(|| Error::from(E_INVALIDARG))?;
        // SAFETY: Composition passes valid out pointers.
        unsafe {
            *index = u32::try_from(position).expect("three properties");
            *mapping = MAPPING_DIRECT;
        }
        Ok(())
    }

    unsafe fn GetPropertyCount(&self, count: *mut u32) -> Result<()> {
        // SAFETY: Composition passes a valid out pointer.
        unsafe { *count = PROPERTIES.len() as u32 };
        Ok(())
    }

    unsafe fn GetProperty(&self, index: u32, value: *mut *mut c_void) -> Result<()> {
        let boxed: IInspectable = match index {
            // No blur: the brush animates the amount.
            0 => IReference::<f32>::from(0.0).into(),
            // D2D1_GAUSSIANBLUR_OPTIMIZATION_BALANCED.
            1 => IReference::<u32>::from(1).into(),
            // D2D1_BORDER_MODE_SOFT: the blur fades out past the edges.
            2 => IReference::<u32>::from(0).into(),
            _ => return Err(E_BOUNDS.into()),
        };
        let property = boxed.cast::<IPropertyValue>()?;
        // SAFETY: Composition passes a valid out pointer and takes the
        // reference.
        unsafe { *value = property.into_raw() };
        Ok(())
    }

    unsafe fn GetSource(&self, index: u32, source: *mut *mut c_void) -> Result<()> {
        if index != 0 {
            return Err(E_BOUNDS.into());
        }
        // SAFETY: as in `GetProperty`.
        unsafe { *source = self.source.clone().into_raw() };
        Ok(())
    }

    unsafe fn GetSourceCount(&self, count: *mut u32) -> Result<()> {
        // SAFETY: Composition passes a valid out pointer.
        unsafe { *count = 1 };
        Ok(())
    }
}
