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

//! UTF-8 encoded text.

use {Error, Result};

#[derive(Copy, Clone)]
pub struct Owned(());

impl<'a> ::traits::Owned<'a> for Owned {
    type Reader = Reader<'a>;
    type Builder = Builder<'a>;
}

pub type Reader<'a> = &'a str;

pub fn new_reader<'a>(v : &'a [u8]) -> Result<Reader<'a>> {
    match ::std::str::from_utf8(v) {
        Ok(v) => Ok(v),
        Err(e) => Err(Error::new_decode_error(
            "Text contains non-utf8 data.", Some(format!("{:?}", e)))),
    }
}

impl <'a> ::traits::FromPointerReader<'a> for Reader<'a> {
    fn get_from_pointer(reader : &::private::layout::PointerReader<'a>) -> Result<Reader<'a>> {
        reader.get_text(::std::ptr::null(), 0)
    }
}

pub struct Builder<'a> {
    bytes : &'a mut [u8],
    pos : usize,
}

impl <'a> Builder <'a> {

    pub fn new<'b>(bytes : &'b mut [u8], pos : u32) -> Result<Builder<'b>> {
        if pos != 0 {
            match ::std::str::from_utf8(bytes) {
                Err(e) => return Err(Error::new_decode_error(
                    "Text contains non-utf8 data.", Some(format!("{:?}", e)))),
                _ => {}
            }
        }
        Ok(Builder { bytes : bytes, pos : pos as usize })
    }

    pub fn push_ascii(&mut self, ascii : u8) {
        assert!(ascii < 128);
        self.bytes[self.pos] = ascii;
        self.pos += 1;
    }

    pub fn push_str(&mut self, string : &str) {
        let bytes = string.as_bytes();
        for ii in 0..bytes.len() {
            self.bytes[self.pos + ii] = bytes[ii];
        }
        self.pos += bytes.len();
    }

    pub fn clear(&mut self) {
        for ii in 0..self.pos {
            self.bytes[ii] = 0;
        }
        self.pos = 0;
    }
}

impl <'a> ::std::ops::Deref for Builder <'a> {
    type Target = str;
    fn deref<'b>(&'b self) -> &'b str {
        ::std::str::from_utf8(self.bytes).unwrap()
    }
}

impl <'a> ::std::convert::AsRef<str> for Builder<'a> {
    fn as_ref<'b>(&'b self) -> &'b str {
        ::std::str::from_utf8(self.bytes).unwrap()
    }
}

impl <'a> ::traits::FromPointerBuilder<'a> for Builder<'a> {
    fn init_pointer(builder : ::private::layout::PointerBuilder<'a>, size : u32) -> Builder<'a> {
        builder.init_text(size)
    }
    fn get_from_pointer(builder : ::private::layout::PointerBuilder<'a>) -> Result<Builder<'a>> {
        builder.get_text(::std::ptr::null(), 0)
    }
}

impl <'a> ::traits::SetPointerBuilder<Builder<'a>> for Reader<'a> {
    fn set_pointer_builder<'b>(pointer : ::private::layout::PointerBuilder<'b>, value : Reader<'a>) -> Result<()> {
        pointer.set_text(value);
        Ok(())
    }
}
