/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Read and write support for Pippin file headers.

use std::io::{Read, Write, ErrorKind};
use std::cmp::min;
use std::result::Result as stdResult;

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};

use PartId;
use detail::readwrite::{sum};
use detail::SUM_BYTES;
use error::{Result, ArgError, ReadError, make_io_err};
use util::rtrim;

// Snapshot header. This is the latest version.
const HEAD_SNAPSHOT : [u8; 16] = *b"PIPPINSS20160227";
// Commit log header. This is the latest version.
const HEAD_COMMITLOG : [u8; 16] = *b"PIPPINCL20160221";
// Versions of header (all versions, including latest), encoded as an integer.
// All restrictions to specific versions should mention `HEAD_VERSIONS` in
// comments to aid searches.
// 
// Note: new versions can be implemented just by updating the three HEAD_...
// constants and updating code, so long as the code will still read old
// versions. The file format documentation should also be updated.
const HEAD_VERSIONS : [u32; 6] = [
    2015_09_29, // initial standardisation
    2016_01_05, // add 'PARTID' to header blocks (snapshot only)
    2016_02_01, // add memory of new names of moved elements
    2016_02_21, // add metadata to commits (logs only)
    2016_02_22, // add metadata to snapshots (snapshots only)
    2016_02_27, // add parent state-sums to snapshots (snapshots only)
];
const SUM_SHA256 : [u8; 16] = *b"HSUM SHA-2 256\x00\x00";
const SUM_BLAKE2_16 : [u8; 16] = *b"HSUM BLAKE2 16\x00\x00";
const PARTID : [u8; 8] = *b"HPARTID ";

/// File type and version.
/// 
/// Version is encoded as an integer; see `HEAD_VERSIONS` constant.
/// 
/// The version is set when a header is read but ignored when the header is
/// written. When creating an instance you can normally just use version 0.
pub enum FileType {
    /// File is a snapshot
    Snapshot(u32),
    /// File is a commit log
    CommitLog(u32),
}
impl FileType {
    /// Extract the version number regardless of file type (should be one of
    /// the HEAD_VERSIONS numbers or zero).
    pub fn ver(&self) -> u32 {
        match self {
            &FileType::Snapshot(v) => v,
            &FileType::CommitLog(v) => v,
        }
    }
}

// Information stored in a file header
pub struct FileHeader {
    /// File type: snapshot or log file.
    pub ftype: FileType,
    /// Repo name. Always present.
    pub name: String,
    /// Partition identifier. Zero if not present.
    pub part_id: Option<PartId>,
    /// User remarks
    pub remarks: Vec<String>,
    /// User data
    pub user_fields: Vec<Vec<u8>>
}

// Decodes from a string to the format used in HEAD_VERSIONS. Returns zero on
// error.
fn read_head_version(s: &[u8]) -> u32 {
    let mut v = 0;
    for c in s {
        if *c < b'0' || *c > b'9' { return 0; }
        v = 10 * v + (*c - b'0') as u32;
    }
    v
}

pub fn validate_repo_name(name: &str) -> stdResult<(), ArgError> {
    if name.len() == 0 {
        return Err(ArgError::new("repo name missing (length 0)"));
    }
    if name.as_bytes().len() > 16 {
        return Err(ArgError::new("repo name too long"));
    }
    Ok(())
}

/// Read a file header.
pub fn read_head(r: &mut Read) -> Result<FileHeader> {
    // A reader which also calculates a checksum:
    let mut sum_reader = sum::HashReader::new(r);
    
    let mut pos: usize = 0;
    let mut buf = vec![0; 32];
    
    try!(sum_reader.read_exact(&mut buf[0..16]));
    let head_version = read_head_version(&buf[8..16]);
    if !HEAD_VERSIONS.contains(&head_version) {
        return ReadError::err("Pippin file of unknown version", pos, (0, 16));
    }
    let ftype = if buf[0..8] == HEAD_SNAPSHOT[0..8] {
        FileType::Snapshot(head_version)
    } else if buf[0..8] == HEAD_COMMITLOG[0..8] {
        FileType::CommitLog(head_version)
    } else {
        return ReadError::err("not a known Pippin file format", pos, (0, 16));
    };
    pos += 16;
    
    try!(sum_reader.read_exact(&mut buf[0..16]));
    let repo_name = match String::from_utf8(rtrim(&buf, 0).to_vec()) {
        Ok(name) => name,
        Err(_) => return ReadError::err("repo name not valid UTF-8", pos, (0, 16))
    };
    pos += 16;
    
    let mut header = FileHeader{
        ftype: ftype,
        name: repo_name,
        part_id: None,
        remarks: Vec::new(),
        user_fields: Vec::new(),
    };
    
    loop {
        try!(sum_reader.read_exact(&mut buf[0..16]));
        let (block, off): (&[u8], usize) = if buf[0] == b'H' {
            pos += 1;
            (&buf[1..16], 1)
        } else if buf[0] == b'Q' {
            let x: usize = match buf[1] {
                b'1' ... b'9' => buf[1] - b'0',
                b'A' ... b'Z' => buf[1] + 10 - b'A',
                _ => return ReadError::err("header section Qx... has invalid length specification 'x'", pos, (0, 2))
            } as usize;
            let len = x * 16;
            if buf.len() < len { buf.resize(len, 0); }
            try!(sum_reader.read_exact(&mut buf[16..len]));
            pos += 2;
            (&buf[2..len], 2)
        } else {
            return ReadError::err("unexpected header contents", pos, (0, 1));
        };
        
        if block[0..3] == *b"SUM" {
            if rtrim(&block[3..], 0) == &SUM_BLAKE2_16[4..14] {
                /* we don't support any other checksum at run-time, so don't need
                 * to configure anything here */
            } else if rtrim(&block[3..], 0) == &SUM_SHA256[4..14] {
                return ReadError::err("file uses SHA256 checksum; program not configured for this",
                    pos, (3+off, 13+off))
            }else {
                return ReadError::err("unknown checksum format", pos, (3+off, 13+off))
            };
            break;      // "HSUM" must be last item of header before final checksum
        } else if block[0..7] == PARTID[1..] {
            if header.part_id != None {
                return ReadError::err("repeat of PARTID", pos, (off, off+7));
            }
            let id = try!((&block[7..15]).read_u64::<BigEndian>());
            if let Some(part_id) = PartId::try_from(id) {
                header.part_id = Some(part_id);
            } else {
                return ReadError::err("invalid partition number", pos, (off+7, off+15));
            };
        } else if block[0] == b'R' {
            header.remarks.push(try!(String::from_utf8(rtrim(&block, 0).to_vec())));
        } else if block[0] == b'U' {
            header.user_fields.push(rtrim(&block[1..], 0).to_vec());
        } else if block[0] == b'O' {
            // Match optional extensions here; we currently have none
        } else if block[0] >= b'A' && block[0] <= b'Z' {
            // Match important extensions here; we currently have none
            // No match:
            // #0017: proper output of warnings
            println!("Warning: unrecognised file extension:");
            println!("{:?}", block);
        } else {
            // Match any other block rules here.
        }
        pos += block.len();
    }
    
    // Read checksum:
    let sum = sum_reader.sum();
    let mut r = sum_reader.into_inner();
    try!(r.read_exact(&mut buf[0..SUM_BYTES]));
    if !sum.eq(&buf[0..SUM_BYTES]) {
        return ReadError::err("header checksum invalid", pos, (0, SUM_BYTES));
    }
    
    Ok(header)
}

/// Write a file header.
pub fn write_head(header: &FileHeader, writer: &mut Write) -> Result<()> {
    // A writer which calculates the checksum of what was written:
    let mut w = sum::HashWriter::new(writer);
    
    match header.ftype {
        // Note: we always write in the latest version, even if we read from an old one
        FileType::Snapshot(_) => {
            try!(w.write(&HEAD_SNAPSHOT));
        },
        FileType::CommitLog(_) => {
            try!(w.write(&HEAD_COMMITLOG));
        },
    };
    try!(validate_repo_name(&header.name));
    let len = try!(w.write(header.name.as_bytes()));
    try!(pad(&mut w, 16 - len));
    
    if let Some(part_id) = header.part_id {
        try!(w.write(&PARTID));
        try!(w.write_u64::<BigEndian>(part_id.into()));
    }
    
    for rem in &header.remarks {
        let b = rem.as_bytes();
        if b[0] != b'R' {
            return ArgError::err("remark does not start 'R'");
        }
        if b.len() <= 15 {
            try!(w.write(b"H"));
            try!(w.write(b));
            try!(pad(&mut w, 15 - b.len()));
        } else if b.len() <= 16 * 36 - 2 {
            let n = (b.len() + 2 /* Qx */ + 15 /* round up */) / 16;
            let l = [b'Q', if n <= 9 { b'0' + n as u8 } else { b'A' - 10 + n as u8 } ];
            try!(w.write(&l));
            try!(w.write(b));
            try!(pad(&mut w, n * 16 - b.len() + 2));
        } else {
            return ArgError::err("remark too long");
        }
    }
    
    for uf in &header.user_fields {
        let mut l = [b'Q', b'H', b'U'];
        if uf.len() <= 14 {
            try!(w.write(&l[1..3]));
            try!(w.write(&uf));
            try!(pad(&mut w, 14 - uf.len()));
        } else if uf.len() <= 16 * 36 - 3 {
            let n = (uf.len() + 3 /* QxU */ + 15 /* round up */) / 16;
            l[1] = if n <= 9 { b'0' + n as u8 } else { b'A' - 10 + n as u8 };
            try!(w.write(&l[0..3]));
            try!(w.write(&uf));
            try!(pad(&mut w, n * 16 - uf.len() - 3));
        } else {
            return ArgError::err("user field too long");
        }
    }
    
    try!(w.write(&SUM_BLAKE2_16));
    
    // Write the checksum of everything above:
    let sum = w.sum();
    try!(sum.write(&mut w.into_inner()));
    
    fn pad<W: Write>(w: &mut W, n1: usize) -> Result<()> {
        let zeros = [0u8; 16];
        let mut n = n1;
        while n > 0 {
            n -= match try!(w.write(&zeros[0..min(n, zeros.len())])) {
                0 => return make_io_err(ErrorKind::WriteZero, "write failed"),
                x => x
            };
        }
        Ok(())
    }
    
    Ok(())
}

#[test]
fn read_header() {
    let head = b"PIPPINSS20160227\
                test AbC \xce\xb1\xce\xb2\xce\xb3\x00\
                HRemark 12345678\
                HOoptional rule\x00\
                HUuser rule\x00\x00\x00\x00\x00\
                Q2REM  completel\
                y pointless text\
                H123456789ABCDEF\
                HSUM BLAKE2 16\x00\x00\
                \xaf\xa8,\x12\x0c\x02\xb7\xb9\xc2\x0eLa\x9b)\x88lnz\x80\xd4e\x80\x96\xa5H0\xb0&H!\xca\xa1";
    
    use ::Sum;
    let sum = Sum::calculate(&head[0..head.len() - SUM_BYTES]);
    println!("Checksum: '{}'", sum.byte_string());
    let header = match read_head(&mut &head[..]) {
        Ok(h) => h,
        Err(e) => { panic!("{}", e); }
    };
    assert_eq!(header.name, "test AbC αβγ");
    assert_eq!(header.remarks, vec!["Remark 12345678", "REM  completely pointless text"]);
    assert_eq!(header.user_fields, vec![b"user rule"]);
}

#[test]
fn write_header() {
    let header = FileHeader {
        ftype: FileType::Snapshot(0 /*version should be ignored*/),
        name: "Ähnliche Unsinn".to_string(),
        part_id: None,
        remarks: vec!["Remark ω".to_string(), "R Quatsch Quatsch Quatsch".to_string()],
        user_fields: vec![b" rsei noasr auyv 10()% xovn".to_vec()]
    };
    let mut buf = Vec::new();
    write_head(&header, &mut buf).unwrap();
    
    let expected = b"PIPPINSS20160227\
            \xc3\x84hnliche Unsinn\
            HRemark \xcf\x89\x00\x00\x00\x00\x00\x00\
            Q2R Quatsch Quatsch \
            Quatsch\x00\x00\x00\x00\x00\x00\x00\x00\x00\
            Q2U rsei noasr a\
            uyv 10()% xovn\x00\x00\
            HSUM BLAKE2 16\x00\x00\
            YN.\x9ft\x11r\x89y`\x9d\x9b\x09\xeb\x83\xbfE\x98M\x9c\x15\x95\x03l;\xe7\xf8\xceLb=\xc0";
    use ::util::ByteFormatter;
    println!("Checksum: '{}'", ByteFormatter::from(&buf[buf.len()-SUM_BYTES..buf.len()]));;
    assert_eq!(&buf[..], &expected[..]);
}
