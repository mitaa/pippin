/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Test Pippin operations on partitions
#![feature(box_syntax)]

extern crate pippin;
extern crate vec_map;
#[macro_use]
extern crate log;
extern crate env_logger;

use std::io::{Read, Write, ErrorKind};
use std::any::Any;
// Used to write results out:
// use std::io::stderr;
// use std::path::Path;
// use std::fs::{File, create_dir_all};

use vec_map::VecMap;

use pippin::PartId;
use pippin::{Partition, PartitionIO};
use pippin::error::{make_io_err, Result};

/// Allows writing to in-memory streams. Refers to external data so that it
/// can be recovered after the `Partition` is destroyed in the tests.
struct PartitionStreams {
    // Map of snapshot-number to pair (snapshot, map of log number to log)
    ss: VecMap<(Vec<u8>, VecMap<Vec<u8>>)>,
}

impl PartitionIO for PartitionStreams {
    fn as_any(&self) -> &Any { self }
    fn ss_len(&self) -> usize {
        self.ss.keys().next_back().map(|x| x+1).unwrap_or(0)
    }
    fn ss_cl_len(&self, ss_num: usize) -> usize {
        match self.ss.get(&ss_num) {
            Some(&(_, ref logs)) => logs.keys().next_back().map(|x| x+1).unwrap_or(0),
            None => 0,
        }
    }
    fn read_ss<'a>(&'a self, ss_num: usize) -> Result<Option<Box<Read+'a>>> {
        Ok(self.ss.get(&ss_num).map(|&(ref data, _)| box &data[..] as Box<Read+'a>))
    }
    fn read_ss_cl<'a>(&'a self, ss_num: usize, cl_num: usize) -> Result<Option<Box<Read+'a>>> {
        Ok(self.ss.get(&ss_num)
            .and_then(|&(_, ref logs)| logs.get(&cl_num))
            .map(|data| box &data[..] as Box<Read+'a>))
    }
    fn new_ss<'a>(&'a mut self, ss_num: usize) -> Result<Option<Box<Write+'a>>> {
        if self.ss.contains_key(&ss_num) {
            Ok(None)
        } else {
            self.ss.insert(ss_num, (Vec::new(), VecMap::new()));
            Ok(Some(Box::new(&mut self.ss.get_mut(&ss_num).unwrap().0)))
        }
    }
    fn append_ss_cl<'a>(&'a mut self, ss_num: usize, cl_num: usize) -> Result<Option<Box<Write+'a>>> {
        if let Some(data) = self.ss.get_mut(&ss_num).and_then(|&mut (_, ref mut logs)| logs.get_mut(&cl_num)) {
            let len = data.len();
            Ok(Some(Box::new(&mut data[len..])))
        } else {
            Ok(None)
        }
    }
    fn new_ss_cl<'a>(&'a mut self, ss_num: usize, cl_num: usize) -> Result<Option<Box<Write+'a>>> {
        if let Some(&mut (_, ref mut logs)) = self.ss.get_mut(&ss_num) {
            if logs.contains_key(&cl_num) {
                return Ok(None);
            }
            logs.insert(cl_num, Vec::new());
            let data = logs.get_mut(&cl_num).unwrap();
            Ok(Some(box &mut *data))
        } else {
            make_io_err(ErrorKind::NotFound, "no snapshot corresponding to new commit log")
        }
    }
}

#[test]
fn create_small() {
    use pippin::State;
    env_logger::init().unwrap();
    
    let part_streams = PartitionStreams { ss: VecMap::new() };
    let part_id = PartId::from_num(56);
    let mut part = Partition::<String>::create_part(box part_streams,
        "create_small", part_id).expect("creating partition");
    
    // 2 Add a few elements over multiple commits
    let mut state = part.tip().expect("has tip").clone_child();
    state.insert("thirty five".to_string()).expect("inserting elt 35");
    state.insert("six thousand, five hundred and thirteen"
            .to_string()).expect("inserting elt 6513");
    state.insert("five million, six hundred and ninety eight \
            thousand, one hundred and thirty one".to_string())
            .expect("inserting elt 5698131");
    part.push_state(state).expect("committing");
    let state1 = part.tip().expect("has tip").clone_exact();
    
    let mut state = part.tip().expect("getting tip").clone_child();
    state.insert("sixty eight thousand, one hundred and sixty eight"
            .to_string()).expect("inserting elt 68168");
    part.push_state(state).expect("committing");
    
    let mut state = part.tip().expect("getting tip").clone_child();
    state.insert("eighty nine".to_string()).expect("inserting elt 89");
    state.insert("one thousand and sixty three".to_string())
            .expect("inserting elt 1063");
    part.push_state(state).expect("committing");
    let state3 = part.tip().expect("has tip").clone_exact();
    
    // 3 Write to streams in memory
    part.write(true).expect("writing");
    let boxed_io = part.unwrap_io();
    
    // 4 Check the generated streams
    {
        let io = boxed_io.as_any().downcast_ref::<PartitionStreams>().expect("downcasting io");
        assert_eq!(io.ss.len(), 1);
        assert!(io.ss.contains_key(&0));
        let &(ref ss_data, ref logs) = io.ss.get(&0).expect("io.ss.get(&0)");
        assert_eq!(logs.len(), 1);
        assert!(logs.contains_key(&0));
        let log = logs.get(&0).expect("logs.get(&0)");
        
        // It is sometimes useful to be able to see these streams. This can be
        // done here:
        use std::path::Path;
        use std::io::stderr;
        use std::fs::{File, create_dir_all};
        let out_path = Path::new("output/partition-ops");
        let fname_ss0 = Path::new("partition-small-ss0.pip");
        let fname_ss0_cl0 = Path::new("partition-small-ss0-cl0.piplog");
        let write_out = |text: &[u8], fname: &Path| -> Result<()> {
            let opath = out_path.join(fname);
            try!(writeln!(stderr(), "Writing create_small() test output to {}", opath.display()));
            create_dir_all(&out_path).expect("create_dir_all");
            let mut of = try!(File::create(opath));
            try!(of.write(text));
            Ok(())
        };
        write_out(&ss_data, &fname_ss0).expect("writing snapshot file");
        write_out(&log, &fname_ss0_cl0).expect("writing commit log");
        
        // We cannot do a binary comparison on the output files since the order
        // in which elements occur can and does vary (thanks to Rust's hash
        // function randomisation). Instead we compare file length here and
        // read the files back below.
        assert_eq!(ss_data.len(), 224);
        assert_eq!(log.len(), 1184);
    }
    
    // 5 Read streams back again and compare
    let mut part2 = Partition::open(boxed_io, part_id);
    part2.load(true).expect("part2.load");
    // The "parent" field is not saved and so unequal in the reloaded state.
    // As a work-around, we use clone_child() which updates the "parent" field.
    assert_eq!(state1.clone_child(),
        part2.state(state1.statesum()).expect("get state1 by sum").clone_child());
    assert_eq!(state3, *part2.tip().expect("part2 tip"));
}
