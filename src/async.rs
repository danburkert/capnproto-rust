// Copyright (c) 2013-2015 Sandstorm Development Group, Inc. and contributors
// Licensed under the MIT License:
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
// THE SOFTWARE.

use std::io::{self, Read};

use {Word, Error, Result};
use private::arena;
use message::ReaderOptions;
use serialize::OwnedSpaceMessageReader;

use byteorder::{ByteOrder, LittleEndian};

macro_rules! try_async {
    ($expr:expr) => (match $expr {
        ::std::result::Result::Ok($crate::async::AsyncValue::Complete(val)) => val,
        ::std::result::Result::Ok($crate::async::AsyncValue::Continue(continuation)) => {
            return ::std::result::Result::Ok($crate::async::AsyncValue::Continue(continuation));
        },
        ::std::result::Result::Err(err) => {
            return ::std::result::Result::Err(::std::convert::From::from(err))
        }
    })
}

/// The value of an async operation. The operation either completed successfuly, signaled by a
/// `Complete` value, or the operation would block and needs to be continued at a later time.
#[derive(Debug)]
pub enum AsyncValue<T, U> {
    Complete(T),
    Continue(U),
}

impl <T, U> AsyncValue<T, U> {
    pub fn unwrap(self) -> T {
        match self {
            AsyncValue::Complete(val) => val,
            AsyncValue::Continue(..) => panic!("called `AsyncValue::unwrap()` on a `Continue` value"),
        }
    }

    pub fn unwrap_continuation(self) -> U {
        match self {
            AsyncValue::Complete(..) => panic!("called `AsyncValue::unwrap_continuation()` on a `Complete` value"),
            AsyncValue::Continue(continuation) => continuation,
        }
    }
}

#[derive(Debug)]
pub struct WriteContinuation {
    idx: usize,
}

#[derive(Debug)]
pub enum ReadContinuation {

    /// Reading the message would block while trying to read the first word (the
    /// segment count, and the first segment's length).
    ///
    /// * `buf` contains the buffer being read into.
    /// * `idx` contains the number of bytes read before being blocked.
    SegmentTableFirst {
        buf: [u8; 8],
        idx: usize,
    },

    /// Reading the message would block while trying to read the rest of the
    /// segment table.
    ///
    /// * `segment_count` contains the total number of segments.
    /// * `first_segment_len` contains the length of the first segment.
    /// * `buf` contains the buffer being read into.
    /// * `idx` contains the number of bytes read before being blocked.
    SegmentTableRest {
        segment_count: usize,
        first_segment_len: usize,
        buf: Box<[u8]>,
        idx: usize,
    },

    /// Reading the message would block while trying to read the segments.
    ///
    /// * `owned_space` contains the segment buffer.
    /// * `idx` contains the number of bytes read into `owned_space` before being blocked.
    Segments {
        segment_slices: Vec<(usize, usize)>,
        owned_space: Vec<Word>,
        idx: usize,
    },
}

pub type AsyncWrite = AsyncValue<(), WriteContinuation>;
pub type AsyncRead = AsyncValue<OwnedSpaceMessageReader, ReadContinuation>;

/// Read a Cap'n Proto serialized message from a stream with the provided options.
pub fn read_message<R>(read: &mut R, options: ReaderOptions) -> Result<AsyncRead>
where R: Read {
    let (segment_count, first_segment_len) = try_async!(read_segment_table_first(read, [0; 8], 0));

    let (total_words, segment_slices) = if segment_count == 1 {
        // if there is only a single segment, then we have already read the whole segment table
        (first_segment_len, vec![(0, first_segment_len)])
    } else {
        // otherwise we read the rest of the segment table
        try_async!(read_segment_table_rest(read,
                                           options,
                                           segment_count,
                                           first_segment_len,
                                           create_segment_table_buf(segment_count),
                                           0))
    };

    read_segments(read,
                  options,
                  segment_slices,
                  Word::allocate_zeroed_vec(total_words),
                  0)
}

/// Reads bytes from `read` into `buf` until either `buf` is full, or the read
/// would block. Returns the number of bytes read.
fn async_read_all<R>(read: &mut R, buf: &mut [u8]) -> io::Result<usize> where R: Read {
    let mut idx = 0;
    while idx < buf.len() {
        let slice = &mut buf[idx..];
        match read.read(slice) {
            Ok(n) if n == 0 => return Err(io::Error::new(io::ErrorKind::Other, "Premature EOF")),
            Ok(n) => idx += n,
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => (),
            Err(e) => return Err(e),
        }
    }
    return Ok(idx)
}

/// Reads or continues reading the first word of a segment table from `read`.
/// Returns the segment count and first segment length, or a continuation if the
/// read would block.
fn read_segment_table_first<R>(read: &mut R,
                               mut buf: [u8; 8],
                               mut idx: usize)
                               -> Result<AsyncValue<(usize, usize), ReadContinuation>>
where R: Read {
    idx += try!(async_read_all(read, &mut buf[idx..]));
    if idx < buf.len() {
        let continuation = ReadContinuation::SegmentTableFirst {
            buf: buf,
            idx: idx,
        };
        return Ok(AsyncValue::Continue(continuation));
    }

    let segment_count = <LittleEndian as ByteOrder>::read_u32(&buf[0..4])
                                                   .wrapping_add(1) as usize;
    if segment_count >= 512 {
        return Err(Error::new_decode_error("Too many segments.",
                                           Some(format!("{}", segment_count))));
    } else if segment_count == 0 {
        return Err(Error::new_decode_error("Too few segments.",
                                           Some(format!("{}", segment_count))));
    }

    let first_segment_len = <LittleEndian as ByteOrder>::read_u32(&buf[4..8]) as usize;
    Ok(AsyncValue::Complete((segment_count, first_segment_len)))
}

/// Reads or continues reading the remaining words (after the first) of a
/// segment table from `read`. Returns the total segment words and segment slices,
/// or a continuation if the read would block.
fn read_segment_table_rest<R>(read: &mut R,
                              options: ReaderOptions,
                              segment_count: usize,
                              first_segment_len: usize,
                              mut buf: Box<[u8]>,
                              mut idx: usize)
                              -> Result<AsyncValue<(usize, Vec<(usize, usize)>), ReadContinuation>>
where R: Read {
    idx += try!(async_read_all(read, &mut buf[idx..]));
    if idx < buf.len() {
        let continuation = ReadContinuation::SegmentTableRest {
            segment_count: segment_count,
            first_segment_len: first_segment_len,
            buf: buf,
            idx: idx,
        };
        return Ok(AsyncValue::Continue(continuation));
    }

    let mut segment_slices: Vec<(usize, usize)> = Vec::with_capacity(segment_count);
    segment_slices.push((0, first_segment_len));
    let mut total_words = first_segment_len;

    for chunk in buf.chunks(4).take(segment_count - 1) {
        let segment_len = <LittleEndian as ByteOrder>::read_u32(chunk) as usize;
        segment_slices.push((total_words, total_words + segment_len));
        total_words += segment_len;
    }

    // Don't accept a message which the receiver couldn't possibly traverse without hitting the
    // traversal limit. Without this check, a malicious client could transmit a very large segment
    // size to make the receiver allocate excessive space and possibly crash.
    if total_words as u64 > options.traversal_limit_in_words  {
        return Err(Error::new_decode_error(
            "Message is too large. To increase the limit on the \
             receiving end, see capnp::ReaderOptions.", Some(format!("{}", total_words))));
    }

    Ok(AsyncValue::Complete((total_words, segment_slices)))
}

/// Reads or continues reading message segments from `read`.
fn read_segments<R>(read: &mut R,
                    options: ReaderOptions,
                    segment_slices: Vec<(usize, usize)>,
                    mut owned_space: Vec<Word>,
                    mut idx: usize)
                    -> Result<AsyncRead>
where R: Read {
    {
        let buf = Word::words_to_bytes_mut(&mut owned_space[..]);
        idx += try!(async_read_all(read, &mut buf[idx..]));
    }
    if idx < owned_space.len() * 8 {
        let continuation = ReadContinuation::Segments {
            segment_slices: segment_slices,
            owned_space: owned_space,
            idx: idx,
        };
        return Ok(AsyncValue::Continue(continuation));
    }

    let arena = {
        let segments = segment_slices.iter()
                                     .map(|&(start, end)| &owned_space[start..end])
                                     .collect::<Vec<_>>();

        arena::ReaderArena::new(&segments[..], options)
    };

    let msg = OwnedSpaceMessageReader {
        options: options,
        arena: arena,
        segment_slices: segment_slices,
        owned_space: owned_space,
    };

    Ok(AsyncValue::Complete(msg))
}

/// Creates a new buffer for reading the segment slice lengths.
fn create_segment_table_buf(segment_count: usize) -> Box<[u8]> {
    // number of segments rounded down to the nearest even number times 4 bytes per value
    let len = (segment_count / 2) * 8;
    vec![0; len].into_boxed_slice()
}

#[cfg(test)]
pub mod test {

    use std::io::{Cursor, Read};

    use quickcheck::{quickcheck, TestResult};

    use {MessageReader, Result, Word};
    use message::ReaderOptions;
    use serialize::test::write_message_segments;
    use super::{
        AsyncValue,
        ReadContinuation,
        create_segment_table_buf,
        read_message,
        read_segment_table_first,
        read_segment_table_rest,
    };

    pub fn read_segment_table<R>(read: &mut R,
                                 options: ReaderOptions)
                                 -> Result<AsyncValue<(usize, Vec<(usize, usize)>), ReadContinuation>>
    where R: Read {
        let (segment_count, first_segment_len) = try_async!(read_segment_table_first(read, [0; 8], 0));

        if segment_count == 1 {
            // if there is only a single segment, then we have already read the whole segment table
            Ok(AsyncValue::Complete((first_segment_len, vec![(0, first_segment_len)])))
        } else {
            // otherwise we read the rest of the segment table
            read_segment_table_rest(read,
                                    options,
                                    segment_count,
                                    first_segment_len,
                                    create_segment_table_buf(segment_count),
                                    0)
        }
    }

    #[test]
    fn test_read_segment_table() {

        let mut buf = vec![];

        buf.extend([0,0,0,0, // 1 segments
                    0,0,0,0] // 0 length
                    .iter().cloned());
        let (words, segment_slices) = read_segment_table(&mut Cursor::new(&buf[..]),
                                                         ReaderOptions::new()).unwrap().unwrap();
        assert_eq!(0, words);
        assert_eq!(vec![(0,0)], segment_slices);
        buf.clear();

        buf.extend([0,0,0,0, // 1 segments
                    1,0,0,0] // 1 length
                    .iter().cloned());
        let (words, segment_slices) = read_segment_table(&mut Cursor::new(&buf[..]),
                                                         ReaderOptions::new()).unwrap().unwrap();
        assert_eq!(1, words);
        assert_eq!(vec![(0,1)], segment_slices);
        buf.clear();

        buf.extend([1,0,0,0, // 2 segments
                    1,0,0,0, // 1 length
                    1,0,0,0, // 1 length
                    0,0,0,0] // padding
                    .iter().cloned());
        let (words, segment_slices) = read_segment_table(&mut Cursor::new(&buf[..]),
                                                         ReaderOptions::new()).unwrap().unwrap();
        assert_eq!(2, words);
        assert_eq!(vec![(0,1), (1, 2)], segment_slices);
        buf.clear();

        buf.extend([2,0,0,0, // 3 segments
                    1,0,0,0, // 1 length
                    1,0,0,0, // 1 length
                    0,1,0,0] // 256 length
                    .iter().cloned());
        let (words, segment_slices) = read_segment_table(&mut Cursor::new(&buf[..]),
                                                         ReaderOptions::new()).unwrap().unwrap();
        assert_eq!(258, words);
        assert_eq!(vec![(0,1), (1, 2), (2, 258)], segment_slices);
        buf.clear();

        buf.extend([3,0,0,0,  // 4 segments
                    77,0,0,0, // 77 length
                    23,0,0,0, // 23 length
                    1,0,0,0,  // 1 length
                    99,0,0,0, // 99 length
                    0,0,0,0]  // padding
                    .iter().cloned());
        let (words, segment_slices) = read_segment_table(&mut Cursor::new(&buf[..]),
                                                         ReaderOptions::new()).unwrap().unwrap();
        assert_eq!(200, words);
        assert_eq!(vec![(0,77), (77, 100), (100, 101), (101, 200)], segment_slices);
        buf.clear();
    }

    #[test]
    fn test_read_invalid_segment_table() {

        let mut buf = vec![];

        buf.extend([0,2,0,0].iter().cloned()); // 513 segments
        buf.extend([0; 513 * 4].iter().cloned());
        assert!(read_segment_table(&mut Cursor::new(&buf[..]),
                                   ReaderOptions::new()).is_err());
        buf.clear();

        buf.extend([0,0,0,0].iter().cloned()); // 1 segments
        assert!(read_segment_table(&mut Cursor::new(&buf[..]),
                                   ReaderOptions::new()).is_err());
        buf.clear();

        buf.extend([0,0,0,0].iter().cloned()); // 1 segments
        buf.extend([0; 3].iter().cloned());
        assert!(read_segment_table(&mut Cursor::new(&buf[..]),
                                   ReaderOptions::new()).is_err());
        buf.clear();

        buf.extend([255,255,255,255].iter().cloned()); // 0 segments
        assert!(read_segment_table(&mut Cursor::new(&buf[..]),
                                   ReaderOptions::new()).is_err());
        buf.clear();
    }

    #[test]
    fn check_round_trip() {
        fn round_trip(segments: Vec<Vec<Word>>) -> TestResult {
            if segments.len() == 0 { return TestResult::discard(); }
            let mut cursor = Cursor::new(Vec::new());

            write_message_segments(&mut cursor, &segments);
            cursor.set_position(0);

            let message = read_message(&mut cursor, ReaderOptions::new()).unwrap().unwrap();

            TestResult::from_bool(segments.iter().enumerate().all(|(i, segment)| {
                &segment[..] == message.get_segment(i)
            }))
        }

        quickcheck(round_trip as fn(Vec<Vec<Word>>) -> TestResult);
    }
}
