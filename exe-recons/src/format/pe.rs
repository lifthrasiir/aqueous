use alloc::{vec, vec::Vec};

use crate::util::{to_u16, to_u32};

pub fn locate_code(f: &mut std::fs::File) -> std::io::Result<Vec<(u64, u64, usize)>> {
    use std::io::{self, BufReader, Read, Seek, SeekFrom};

    f.seek(SeekFrom::Start(0))?;
    let mut r = BufReader::new(f);

    // MZ header
    let mut buf = [0u8; 0x40];
    r.read_exact(&mut buf)?;
    let sig = to_u16(&buf[0..2]);
    if sig != 0x5a4d {
        return Err(io::Error::other(format!("bad MZ signature {sig:#x}")));
    }
    let pe_offset = to_u32(&buf[0x3c..0x40]);
    if pe_offset < 0x40 {
        return Err(io::Error::other(format!(
            "too low offset to PE header {pe_offset:#x}"
        )));
    }

    r.seek_relative(pe_offset as i64 - 0x40)?;

    // PE header
    let mut buf = [0u8; 0x18];
    r.read_exact(&mut buf)?;
    let sig = to_u32(&buf[0..4]);
    if sig != 0x4550 {
        return Err(io::Error::other(format!("bad PE signature {sig:#x}")));
    }
    let num_sections = to_u16(&buf[6..8]);
    let opt_header_size = to_u16(&buf[0x14..0x16]) as usize;

    // PE optional header
    let mut opt_header = vec![0u8; opt_header_size];
    r.read_exact(&mut opt_header)?;
    let magic = to_u16(&opt_header[0..2]);
    match magic {
        0x10b => {
            // IMAGE_OPTIONAL_HEADER32
            todo!();
        }

        0x20b => {
            // IMAGE_OPTIONAL_HEADER64
            if opt_header_size < 0x60 {
                return Err(io::Error::other(format!(
                    "PE64 optional header too small ({opt_header_size:#x} < 0x60)"
                )));
            }

            let num_data_dirs = to_u32(&opt_header[0x5c..0x60]) as usize;
            let min_size = 0x60 + num_data_dirs * 0x10;
            if opt_header_size < min_size {
                return Err(io::Error::other(format!(
                    "PE64 optional header too small ({opt_header_size:#x} < {min_size:#x})"
                )));
            }

            // data directories:
            // EXPORT, IMPORT, RESOURCE, EXCEPTION, SECURITY, BASERELOC, DEBUG, COPYRIGHT,
            // GLOBALPTR, TLS, LOAD_CONFIG, BOUND_IMPORT, IAT, DELAY_IMPORT, COM_DESCRIPTOR, -
        }

        _ => {
            return Err(io::Error::other(format!(
                "bad PE optional header magic {magic:#x}"
            )))
        }
    }

    // section headers
    let mut exec_sections = Vec::new();
    for _ in 0..num_sections {
        let mut buf = [0u8; 40];
        r.read_exact(&mut buf)?;
        let name = &buf[..8];
        let _name = String::from_utf8_lossy(name);
        let rva = to_u32(&buf[12..16]);
        let stored_size = to_u32(&buf[16..20]);
        let stored_offset = to_u32(&buf[20..24]);
        let flags = to_u32(&buf[36..40]);
        //println!("section {_name:?}: rva {rva:#x}, stored {stored_offset:#x} + {stored_size:#x}, flags {flags:#x}");
        if flags & 0x20 != 0 {
            exec_sections.push((rva as u64, stored_offset as u64, stored_size as usize));
        }
    }

    Ok(exec_sections)
}
