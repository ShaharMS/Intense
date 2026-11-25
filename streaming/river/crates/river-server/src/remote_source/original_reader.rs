//! A codec cursor uses original object offsets and the same conditional
//! range reads, cancellation and wire allowance as packet preparation.
use super::{ReadContext, RemoteSource};
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Mutex;

pub(crate) struct OriginalRead(pub Mutex<ReadContext>);
impl std::fmt::Debug for OriginalRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OriginalRead")
    }
}

pub(crate) struct Cursor<'a> {
    source: &'a RemoteSource,
    context: &'a mut ReadContext,
    offset: u64,
    pub error: Option<String>,
}

impl RemoteSource {
    pub(crate) fn fits_original_block(&self) -> bool {
        self.meta.size <= self.block_size
    }

    pub(crate) fn cursor<'a>(&'a self, context: &'a mut ReadContext) -> Cursor<'a> {
        Cursor {
            source: self,
            context,
            offset: 0,
            error: None,
        }
    }

    pub(crate) fn read_at(
        &self,
        offset: u64,
        out: &mut [u8],
        context: &mut ReadContext,
    ) -> anyhow::Result<usize> {
        let len = self.meta.size.saturating_sub(offset).min(out.len() as u64) as usize;
        if len == 0 {
            return Ok(0);
        }
        out[..len].copy_from_slice(&self.read(offset..offset + len as u64, context)?);
        Ok(len)
    }
}

impl Read for Cursor<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self
            .source
            .read_at(self.offset, out, self.context)
            .map_err(|e| {
                self.error = Some(format!("{e:#}"));
                io::Error::other(e)
            })?;
        self.offset += n as u64;
        Ok(n)
    }
}

impl Seek for Cursor<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let next = match position {
            SeekFrom::Start(pos) => i128::from(pos),
            SeekFrom::End(delta) => i128::from(self.source.meta.size) + i128::from(delta),
            SeekFrom::Current(delta) => i128::from(self.offset) + i128::from(delta),
        };
        if !(0..=i128::from(self.source.meta.size)).contains(&next) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek outside original image",
            ));
        }
        self.offset = next as u64;
        Ok(self.offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_source::{CacheState, RangeCache};
    use object_store::{memory::InMemory, path::Path, ObjectStore, ObjectStoreExt};
    use std::sync::Arc;

    #[tokio::test]
    async fn original_offsets_are_repeatable_and_coefficients_are_version_scoped() {
        let data = include_bytes!("../../tests/fixtures/tiny-groups16-kdu444/gray16.jp2");
        let path = Path::from("gray.jp2");
        let store = Arc::new(InMemory::new());
        store.put(&path, data.as_slice().into()).await.unwrap();
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let cache = Arc::new(RangeCache {
                limit: 1 << 20,
                state: Mutex::new(CacheState::default()),
            });
            let open = || {
                let meta = runtime.block_on(store.head(&path)).unwrap();
                let id = format!("cursor-test-{}", meta.e_tag.as_deref().unwrap());
                RemoteSource::open(
                    "test".into(),
                    id,
                    store.clone(),
                    path.clone(),
                    meta,
                    runtime.clone(),
                    cache.clone(),
                )
                .unwrap()
                .0
            };
            let source = open();
            let mut context = ReadContext::new(None);
            let mut cursor = source.cursor(&mut context);
            let mut sample = [0; 17];
            for pos in [511, 23, 4096, 511] {
                cursor.seek(SeekFrom::Start(pos)).unwrap();
                cursor.read_exact(&mut sample).unwrap();
                assert_eq!(&sample[..], &data[pos as usize..pos as usize + 17]);
            }
            assert!(cursor.seek(SeekFrom::End(1)).is_err());
            cursor.seek(SeekFrom::End(0)).unwrap();
            assert_eq!(cursor.read(&mut sample).unwrap(), 0);
            let image = jpeg2k::Image::from_reader_with(
                &mut cursor,
                data.len() as u64,
                jpeg2k::DecodeParameters::new(),
            )
            .unwrap();
            let reference =
                jpeg2k::Image::from_bytes_with(data, jpeg2k::DecodeParameters::new()).unwrap();
            assert_eq!(
                image.components()[0].data(),
                reference.components()[0].data()
            );
            assert_eq!(source.block_namespace, open().block_namespace);
            runtime
                .block_on(store.put(&path, data.as_slice().into()))
                .unwrap();
            assert_ne!(source.block_namespace, open().block_namespace);
        })
        .await
        .unwrap();
    }
}
