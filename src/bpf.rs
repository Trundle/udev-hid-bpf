// SPDX-License-Identifier: GPL-2.0-only

include!(concat!(env!("OUT_DIR"), "/attach.skel.rs"));

use crate::hidudev;
use anyhow::{bail, Context, Result};
use libbpf_rs::skel::{OpenSkel, SkelBuilder};
use libbpf_rs::{AsRawLibbpf, Btf, Object, OpenObject, Program};
use std::convert::TryInto;
use std::fmt::Display;
use std::fs;
use std::os::fd::{AsFd, AsRawFd};
use std::os::raw::{c_int, c_uchar, c_uint};
use std::path::Path;
use std::sync::OnceLock;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
#[allow(non_camel_case_types)]
pub struct hid_bpf_probe_args {
    pub hid: c_uint,
    pub rdesc_size: c_uint,
    pub rdesc: [c_uchar; 4096usize],
    pub retval: c_int,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct AttachProgArgs {
    pub prog_fd: c_int,
    pub hid: c_uint,
    pub retval: c_int,
}

#[derive(Debug)]
pub enum BpfError {
    LibBPFError { error: libbpf_rs::Error },
    OsError { errno: u32 },
    Unsupported,
}

impl std::error::Error for BpfError {}

impl Display for BpfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BpfError::LibBPFError { error } => write!(f, "{error}"),
            BpfError::OsError { errno } => {
                write!(f, "{}", libbpf_rs::Error::from_raw_os_error(*errno as i32))
            }
            BpfError::Unsupported => write!(f, "unsupported on this kernel"),
        }
    }
}

impl From<libbpf_rs::Error> for BpfError {
    fn from(e: libbpf_rs::Error) -> BpfError {
        BpfError::LibBPFError { error: e }
    }
}

pub struct HidBPF {}

pub trait HidBPFLoader {
    fn load(&self, object: OpenObject, _device: &hidudev::HidUdev) -> Result<Object, BpfError> {
        Ok(object.load()?)
    }

    fn probe(&self, object: &Object, device: &hidudev::HidUdev) -> Result<i32, BpfError> {
        match object.prog("probe") {
            None => Ok(0),
            Some(probe) => {
                let args = hid_bpf_probe_args::from(device);
                run_syscall_prog_probe(probe, args)
            }
        }
    }

    fn inject_udev_properties_in_array(
        &self,
        object: &mut Object,
        array_name: &str,
        btf: &Btf,
        udev_properties: &[hidudev::HidUdevProperty],
    ) -> Result<(), BpfError> {
        let Some(btf_map) = btf.type_by_name::<libbpf_rs::btf::types::DataSec>(array_name) else {
            return Ok(());
        };

        object
            .maps_iter_mut()
            .filter(|m| m.name().ends_with(array_name))
            .for_each(|m| {
                m.keys().for_each(|k| {
                    if let Some(mut data) = m.lookup(&k, libbpf_rs::MapFlags::ANY).unwrap() {
                        if btf_map.iter().fold(false, |acc, v| {
                            let v_type = btf.type_by_id::<libbpf_rs::btf::BtfType>(v.ty).unwrap();

                            v_type
                                .name()
                                .map(|n| n.to_str().unwrap())
                                .filter(|name| name.starts_with("UDEV_PROP_"))
                                .map(|name| &name["UDEV_PROP_".len()..])
                                .and_then(|pname| {
                                    udev_properties.iter().find(|prop| prop.name == pname)
                                })
                                .map_or(false, |prop| {
                                    let buf = prop.value.as_bytes();

                                    let size_ok = buf.len() < v.size;

                                    if size_ok {
                                        let start: usize = v.offset.try_into().unwrap();
                                        let end = start + buf.len();

                                        data[start..end].clone_from_slice(buf);

                                        log::debug!(target: "libbpf",
                                                   "inserting {}={} in map {}", prop.name, prop.value, m.name());
                                    }

                                    size_ok
                                })
                                || acc
                        }) {
                            let r = m.update(&k, &data, libbpf_rs::MapFlags::ANY);
                            log::debug!(target: "libbpf",
                                        "updated map {}: {:?}", m.name(), r);
                        }
                    }
                });
            });
        Ok(())
    }

    fn inject_udev_properties(
        &self,
        object: &mut Object,
        device: &hidudev::HidUdev,
        extra_props: &[hidudev::HidUdevProperty],
    ) -> Result<(), BpfError> {
        let btf = Btf::from_bpf_object(unsafe { object.as_libbpf_object().as_ref() })?.unwrap();

        let udev_properties: Vec<hidudev::HidUdevProperty> = device
            .udev_properties()
            .into_iter()
            .filter(|prop| !extra_props.iter().any(|ep| ep.name == prop.name))
            .chain(extra_props.iter().map(hidudev::HidUdevProperty::from))
            .collect();

        self.inject_udev_properties_in_array(object, ".bss", &btf, &udev_properties)?;
        self.inject_udev_properties_in_array(object, ".data", &btf, &udev_properties)?;
        Ok(())
    }

    fn attach_and_pin(
        &self,
        object: &mut Object,
        device: &hidudev::HidUdev,
        bpffs_path: &str,
    ) -> Result<Vec<String>, BpfError>;
}

pub struct HidBPFTrace<'a> {
    inner: Option<AttachSkel<'a>>,
}

#[derive(Default)]
pub struct HidBPFStructOps {}

pub fn get_bpffs_path(sysname: &str, object: &str) -> String {
    format!(
        "/sys/fs/bpf/hid/{}/{}",
        sysname.replace([':', '.'], "_"),
        object.replace([':', '.'], "_"),
    )
}

pub fn remove_bpf_objects(sysname: &str) -> std::io::Result<()> {
    let path = get_bpffs_path(sysname, "");

    std::fs::remove_dir_all(path).ok();

    Ok(())
}

fn run_syscall_prog_generic<T>(prog: &libbpf_rs::Program, data: T) -> Result<T, BpfError> {
    let fd = prog.as_fd().as_raw_fd();
    let data_ptr: *const libc::c_void = &data as *const _ as *const libc::c_void;
    let mut run_opts = libbpf_sys::bpf_test_run_opts {
        sz: std::mem::size_of::<libbpf_sys::bpf_test_run_opts>()
            .try_into()
            .unwrap(),
        ctx_in: data_ptr,
        ctx_size_in: std::mem::size_of::<T>() as u32,
        ..Default::default()
    };

    let run_opts_ptr: *mut libbpf_sys::bpf_test_run_opts = &mut run_opts;

    match unsafe { libbpf_sys::bpf_prog_test_run_opts(fd, run_opts_ptr) } {
        0 => Ok(data),
        e => Err(BpfError::OsError { errno: -e as u32 }),
    }
}

fn run_syscall_prog_attach(
    prog: &libbpf_rs::Program,
    attach_args: AttachProgArgs,
) -> Result<i32, BpfError> {
    let args = run_syscall_prog_generic(prog, attach_args)?;
    if args.retval < 0 {
        Err(BpfError::OsError {
            errno: -args.retval as u32,
        })
    } else {
        Ok(args.retval)
    }
}

fn run_syscall_prog_probe(
    prog: &libbpf_rs::Program,
    probe_args: hid_bpf_probe_args,
) -> Result<i32, BpfError> {
    let args = run_syscall_prog_generic(prog, probe_args)?;
    if args.retval != 0 {
        Err(BpfError::OsError {
            errno: -args.retval as u32,
        })
    } else {
        Ok(args.retval)
    }
}

/*
* We have to rewrite our own `pin()` because we must be pinning the link
* provided by HID-BPF, not the Program object nor a normal libbpf_rs::Link
*/
fn pin_hid_bpf_prog(link: i32, path: &str) -> Result<(), BpfError> {
    unsafe {
        let c_str = std::ffi::CString::new(path).unwrap();

        match libbpf_sys::bpf_obj_pin(link, c_str.as_ptr()) {
            0 => Ok(()),
            e => Err(BpfError::OsError { errno: -e as u32 }),
        }
    }
}

impl hid_bpf_probe_args {
    fn from(device: &hidudev::HidUdev) -> Self {
        let syspath = device.syspath();
        let rdesc = syspath + "/report_descriptor";

        let mut buffer = fs::read(rdesc).unwrap();
        let length = buffer.len();

        buffer.resize(4096, 0);

        hid_bpf_probe_args {
            hid: device.id(),
            rdesc_size: length as u32,
            rdesc: buffer.try_into().unwrap(),
            retval: -1,
        }
    }
}

impl<'a> Default for HidBPFTrace<'a> {
    fn default() -> Self {
        let skel_builder = AttachSkelBuilder::default();

        if let Ok(open_skel) = skel_builder.open() {
            if let Ok(inner) = open_skel.load() {
                return Self { inner: Some(inner) };
            }
        }

        Self { inner: None }
    }
}

impl<'a> HidBPFTrace<'a> {
    fn load_prog(&self, prog: &Program, hid_id: u32, bpffs_path: &str) -> Result<String> {
        let inner = self.inner.as_ref().expect("open_and_load() never called!");
        let attach_args = AttachProgArgs {
            prog_fd: prog.as_fd().as_raw_fd(),
            hid: hid_id,
            retval: -1,
        };

        let link = run_syscall_prog_attach(inner.progs().attach_prog(), attach_args)
            .context(format!("failed the syscall for {}", prog.name()))?;

        log::debug!(
            target: "libbpf",
            "successfully attached {} to device id {}",
            &prog.name(),
            hid_id,
        );

        let path = format!("{}/{}", bpffs_path, prog.name(),);

        fs::create_dir_all(bpffs_path).unwrap_or_else(|why| {
            log::warn!("! {:?}", why.kind());
        });

        pin_hid_bpf_prog(link, &path).context(format!(
            "could not pin {} to device id {}",
            &prog.name(),
            hid_id
        ))?;

        log::debug!(target: "libbpf", "Successfully pinned prog at {}", path);

        Ok(path)
    }

    fn load_progs(
        &self,
        object: &Object,
        hid_id: u32,
        bpffs_path: &str,
    ) -> Result<Vec<String>, BpfError> {
        let attached: Vec<String> = object
            .progs_iter()
            .filter(|p| matches!(p.prog_type(), libbpf_rs::ProgramType::Tracing))
            .map(|p| self.load_prog(p, hid_id, bpffs_path))
            .inspect(|r| {
                if let Err(e) = r {
                    log::warn!("failed to attach to device id {}: {:#}", hid_id, e,);
                }
            })
            .flatten()
            .collect();

        if attached.is_empty() {
            Err(BpfError::OsError {
                errno: libc::EINVAL as u32,
            })
        } else {
            Ok(attached)
        }
    }
}

impl<'a> HidBPFLoader for HidBPFTrace<'a> {
    fn load(&self, object: OpenObject, _device: &hidudev::HidUdev) -> Result<Object, BpfError> {
        match self.inner {
            None => Err(BpfError::OsError {
                errno: libc::ENOTSUP as u32,
            }),
            Some(_) => Ok(object.load()?),
        }
    }
    fn attach_and_pin(
        &self,
        object: &mut Object,
        device: &hidudev::HidUdev,
        bpffs_path: &str,
    ) -> Result<Vec<String>, BpfError> {
        let hid_id = device.id();

        self.load_progs(object, hid_id, bpffs_path)
    }
}

impl HidBPFLoader for HidBPFStructOps {
    fn load(
        &self,
        mut open_object: OpenObject,
        device: &hidudev::HidUdev,
    ) -> Result<Object, BpfError> {
        let bytes_hid_id: [u8; 4] = device.id().to_le_bytes();

        open_object
            .maps_iter_mut()
            .filter(|m| matches!(m.map_type(), libbpf_rs::MapType::StructOps))
            .for_each(|m| {
                if let Some(data) = m.initial_value_mut() {
                    data[0..4].copy_from_slice(&bytes_hid_id);
                }
            });

        open_object.load().map_err(|e| {
            // Unfortunately libbpf gives us ENOENT if the kernel does
            // not support struct ops which makes it impossible to distinguish
            // between "not supported" and "file not found".
            // Since we do our best to only ever load files that exist
            // in the fs, let's assume ENOENT here means "not supported".
            // Which is a much better error than "no such file or directory" for an
            // object that definitely exists...
            if e.kind() == libbpf_rs::ErrorKind::NotFound {
                BpfError::Unsupported
            } else {
                BpfError::LibBPFError { error: e }
            }
        })
    }

    fn attach_and_pin(
        &self,
        object: &mut Object,
        _device: &hidudev::HidUdev,
        bpffs_path: &str,
    ) -> Result<Vec<String>, BpfError> {
        fs::create_dir_all(bpffs_path).unwrap_or_else(|why| {
            log::warn!("! {:?}", why.kind());
        });

        object
            .maps_iter_mut()
            .filter(|m| matches!(m.map_type(), libbpf_rs::MapType::StructOps))
            .map(|m| {
                let path = format!("{}/{}", bpffs_path, m.name());

                m.attach_struct_ops()?.pin(&path)?;
                Ok(path)
            })
            .collect()
    }
}

fn get_bpf_loader(open_object: &OpenObject) -> &'static dyn HidBPFLoader {
    static HID_BPF_TRACE: OnceLock<HidBPFTrace> = OnceLock::new();
    static HID_BPF_STRUCT_OPS: OnceLock<HidBPFStructOps> = OnceLock::new();

    let have_struct_ops: bool = open_object.progs_iter().any(|p| {
        matches!(p.prog_type(), libbpf_rs::ProgramType::StructOps)
            && p.section().starts_with("struct_ops/hid_")
    });

    if !have_struct_ops {
        log::debug!("Using HID_BPF_TRACE");
        HID_BPF_TRACE.get_or_init(HidBPFTrace::default)
    } else {
        log::debug!("Using HID_BPF_STRUCT_OPS");
        HID_BPF_STRUCT_OPS.get_or_init(HidBPFStructOps::default)
    }
}

impl HidBPF {
    fn pin_maps(object: &mut Object, bpffs_path: &String) -> Result<()> {
        // compiler internal maps contain the name of the object and a dot
        for map in object
            .maps_iter_mut()
            .filter(|map| !map.name().contains('.'))
            .filter(|m| !matches!(m.map_type(), libbpf_rs::MapType::StructOps))
        {
            let path = format!("{}/{}", bpffs_path, map.name(),);

            map.pin(&path)
                .context(format!("Failed to pin map at {}", path))?;
            log::debug!(target: "libbpf", "Successfully pinned map at {}", path);
        }

        Ok(())
    }

    pub fn load_programs(
        path: &Path,
        device: &hidudev::HidUdev,
        properties: &[hidudev::HidUdevProperty],
    ) -> Result<()> {
        log::debug!(target: "libbpf", "loading BPF object at {:?}", path.display());

        let mut obj_builder = libbpf_rs::ObjectBuilder::default();
        let open_object = obj_builder.open_file(path)?;

        let loader = get_bpf_loader(&open_object);

        let mut object = loader.load(open_object, device)?;
        let object_name = path.file_stem().unwrap().to_str().unwrap();

        loader
            .inject_udev_properties(&mut object, device, properties)
            .context(format!("couldn't set udev properties on {object_name}"))?;

        /*
         * if there is a "probe" syscall, execute it and
         * check for the return value: if not 0, then ignore
         * this bpf.o file
         */
        loader
            .probe(&object, device)
            .context(format!("probe() of {object_name} failed"))?;

        let bpffs_path = get_bpffs_path(&device.sysname(), object_name);
        loader
            .attach_and_pin(&mut object, device, &bpffs_path)
            .context(format!("attach_and_pin() of {object_name} failed"))?;

        if let Err(e) = HidBPF::pin_maps(&mut object, &bpffs_path) {
            let _ = std::fs::remove_dir_all(bpffs_path);
            bail!(e);
        };

        Ok(())
    }
}
