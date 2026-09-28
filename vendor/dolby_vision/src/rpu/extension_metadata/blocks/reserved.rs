use anyhow::{Result, bail, ensure};

use bitvec_helpers::{
    bitstream_io_reader::BsIoSliceReader, bitstream_io_writer::BitstreamIoWriter,
};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use super::{ExtMetadataBlock, ExtMetadataBlockInfo};

#[derive(Debug, Default, Clone)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
pub struct ReservedExtMetadataBlock {
    pub ext_block_length: u64,
    pub ext_block_level: u8,

    #[cfg_attr(feature = "serde", serde(skip_deserializing))]
    pub data: Vec<u8>,
}

impl ReservedExtMetadataBlock {
    pub(crate) fn parse(
        ext_block_length: u64,
        ext_block_level: u8,
        reader: &mut BsIoSliceReader,
    ) -> Result<ExtMetadataBlock> {
        // PLURX-PATCH 1: the length is a ue(v) from the bitstream; refuse one
        // longer than the bytes left before allocating for it.
        let available_bytes = reader.available()? / 8;
        ensure!(
            ext_block_length <= available_bytes,
            "reserved block length {} exceeds the {} bytes left in the RPU",
            ext_block_length,
            available_bytes
        );
        let mut data = vec![0; ext_block_length as usize];
        reader.read_bytes(&mut data)?;

        Ok(ExtMetadataBlock::Reserved(Self {
            ext_block_length,
            ext_block_level,
            data,
        }))
    }

    pub fn write(&self, _writer: &mut BitstreamIoWriter) -> Result<()> {
        bail!("Cannot write reserved block");
    }
}

impl ExtMetadataBlockInfo for ReservedExtMetadataBlock {
    fn level(&self) -> u8 {
        0
    }

    fn bytes_size(&self) -> u64 {
        self.ext_block_length
    }

    fn required_bits(&self) -> u64 {
        self.ext_block_length * 8
    }
}
