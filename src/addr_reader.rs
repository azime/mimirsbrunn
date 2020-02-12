use crate::Error;
use csv;
use failure::ResultExt;
use libflate::gzip;
use mimir::rubber::{IndexSettings, IndexVisibility, Rubber};
use mimir::Addr;
use par_map::ParMap;
use serde::de::DeserializeOwned;
use slog_scope::{error, info, warn};
use std::fs::File;
use std::io::Read;
use std::marker::{Send, Sync};
use std::path::PathBuf;

pub fn import_addresses<T, F>(
    rubber: &mut Rubber,
    has_headers: bool,
    with_gzip: bool,
    nb_threads: usize,
    index_settings: IndexSettings,
    dataset: &str,
    files: impl IntoIterator<Item = PathBuf>,
    into_addr: F,
) -> Result<(), Error>
where
    F: Fn(T) -> Result<Addr, Error> + Send + Sync + 'static,
    T: DeserializeOwned + Send + 'static,
{
    let addr_index = rubber
        .make_index(dataset, &index_settings)
        .with_context(|_| format!("Error occurred when making index {}", dataset))?;
    info!("Add data in elasticsearch db.");

    let iter = files
        .into_iter()
        .filter_map(|path| {
            info!("importing {:?}...", &path);
            File::open(&path)
                .map_err(|err| error!("impossible to read file {:?}, error: {}", path, err))
                .ok()
        })
        .filter_map(|file| {
            if with_gzip {
                gzip::Decoder::new(file)
                    .map_err(|err| error!("impossible to read gzip in, error: {}", err))
                    .map(|decoder| Box::new(decoder) as Box<dyn Read>)
                    .ok()
            } else {
                Some(Box::new(file) as Box<dyn Read>)
            }
        })
        .flat_map(|stream| {
            csv::ReaderBuilder::new()
                .has_headers(has_headers)
                .from_reader(stream)
                .into_deserialize()
        })
        .filter_map(|line| {
            line.map_err(|e| warn!("impossible to read line, error: {}", e))
                .ok()
        })
        .with_nb_threads(nb_threads)
        .par_map(into_addr)
        .filter_map(|ra| match ra {
            Ok(a) => {
                if a.street.name.is_empty() {
                    warn!("Address {} has no street name and has been ignored.", a.id);
                    None
                } else {
                    Some(a)
                }
            }
            Err(err) => {
                warn!("Address Error ignored: {}", err);
                None
            }
        });
    let nb = rubber
        .bulk_index(&addr_index, iter)
        .with_context(|_| format!("failed to bulk insert"))?;
    info!("importing addresses: {} addresses added.", nb);

    rubber
        .publish_index(dataset, addr_index, IndexVisibility::Public)
        .context("Error while publishing the index")?;
    Ok(())
}
