//! fn-matshader: the original gun materials (`WeaponMat_Base`, `BackPackShrinkingEffect` shader maps) recovered from the cooked 1.8 data and run on the GPU.
//!
//! * [`maps`]      the six `FMaterialShaderMap`s of a cooked material (quality x feature level), the programs each owns with shader type + vertex factory
//! * [`material`]  a material instance chain: effective parameters, referenced textures, render flags, the SM5 map to run, the packed Material constant buffer
//! * [`mesh`]      raw GPU-skin vertex streams of a skeletal mesh and the per-section bone maps
//! * [`assemble`]  DXBC -> WGSL assembly of a (vertex, pixel) program pair into one module with the renderer's gun resolve
//! * [`view`], [`frame`] the `View` / `Primitive` / `PrimitiveFade` / vertex factory buffers and bone matrices in Unreal conventions
//! * [`textures`]  texture upload and sampler state of the material's sampler members
//! * [`gun`]       permutation selection and the [`gun::GunModel`] the application drives each frame
pub mod assemble;
pub mod frame;
pub mod gun;
pub mod maps;
pub mod material;
pub mod mesh;
pub mod textures;
pub mod view;
pub mod scope;
pub mod decal;
pub mod outline; // OUTLINE: the build preview highlight (SkinAndMeshOutlinePostProcess + custom depth), outline.rs

pub mod storm; // STORM HOOK: safe-zone wall + storm cloud sheet through the FX pass (storm.rs)
