mod bitcoind;
mod database;
mod jsonrpc;
mod revaultd;
mod threadmessages;

use crate::{
    bitcoind::actions::{bitcoind_main_loop, start_bitcoind},
    database::actions::setup_db,
    jsonrpc::{jsonrpcapi_loop, jsonrpcapi_setup},
    revaultd::RevaultD,
    threadmessages::*,
};
use common::config::Config;
use database::interface::db_tip;

use std::{
    env,
    path::PathBuf,
    process,
    str::FromStr,
    sync::{mpsc, Arc, RwLock},
    thread,
};

use daemonize_simple::Daemonize;

fn parse_args(args: Vec<String>) -> Option<PathBuf> {
    if args.len() == 1 {
        return None;
    }

    if args.len() != 3 {
        eprintln!("Unknown arguments '{:?}'.", args);
        eprintln!("Only '--conf <configuration file path>' is supported.");
        process::exit(1);
    }

    Some(PathBuf::from(args[2].to_owned()))
}

fn daemon_main(mut revaultd: RevaultD) {
    let (db_path, network) = (revaultd.db_file(), revaultd.bitcoind_config.network);

    // First and foremost
    log::info!("Setting up database");
    setup_db(&mut revaultd).unwrap_or_else(|e| {
        log::error!("Error setting up database: '{}'", e.to_string());
        process::exit(1);
    });

    log::info!("Setting up bitcoind connection");
    let bitcoind = start_bitcoind(&mut revaultd).unwrap_or_else(|e| {
        log::error!("Error setting up bitcoind: {}", e.to_string());
        process::exit(1);
    });

    log::info!("Starting JSONRPC server");
    let socket = jsonrpcapi_setup(revaultd.rpc_socket_file()).unwrap_or_else(|e| {
        log::error!("Setting up JSONRPC server: {}", e.to_string());
        process::exit(1);
    });

    // We start two threads, the JSONRPC one in order to be controlled externally,
    // and the bitcoind one to poll bitcoind until we die.
    // We may get requests from the RPC one, and send requests to the bitcoind one.

    // The communication from them to us
    let (rpc_tx, rpc_rx) = mpsc::channel();

    // The communication from us to the bitcoind thread
    let (bitcoind_tx, bitcoind_rx) = mpsc::channel();

    let jsonrpc_thread = thread::spawn(move || {
        jsonrpcapi_loop(rpc_tx, socket).unwrap_or_else(|e| {
            log::error!("Error in JSONRPC server event loop: {}", e.to_string());
            process::exit(1)
        })
    });

    let revaultd = Arc::new(RwLock::new(revaultd));
    let bit_revaultd = revaultd.clone();
    let bitcoind_thread = thread::spawn(move || {
        bitcoind_main_loop(bitcoind_rx, bit_revaultd, &bitcoind).unwrap_or_else(|e| {
            log::error!("Error in bitcoind main loop: {}", e.to_string());
            process::exit(1)
        })
    });

    log::info!(
        "revaultd started on network {}",
        revaultd.read().unwrap().bitcoind_config.network
    );
    for message in rpc_rx {
        match message {
            RpcMessageIn::Shutdown => {
                log::info!("Stopping revaultd.");
                bitcoind_tx
                    .send(BitcoindMessageOut::Shutdown)
                    .unwrap_or_else(|e| {
                        log::error!("Sending shutdown to bitcoind thread: {:?}", e);
                        process::exit(1);
                    });

                jsonrpc_thread.join().unwrap_or_else(|e| {
                    log::error!("Joining RPC server thread: {:?}", e);
                    process::exit(1);
                });
                bitcoind_thread.join().unwrap_or_else(|e| {
                    log::error!("Joining bitcoind thread: {:?}", e);
                    process::exit(1);
                });
                process::exit(0);
            }
            RpcMessageIn::GetInfo(response_tx) => {
                log::trace!("Got getinfo from RPC thread");

                let (bitrep_tx, bitrep_rx) = mpsc::sync_channel(0);
                bitcoind_tx
                    .send(BitcoindMessageOut::SyncProgress(bitrep_tx))
                    .unwrap_or_else(|e| {
                        log::error!("Sending 'syncprogress' to bitcoind thread: {:?}", e);
                        process::exit(1);
                    });
                let progress = bitrep_rx.recv().unwrap_or_else(|e| {
                    log::error!("Receving 'syncprogress' from bitcoind thread: {:?}", e);
                    process::exit(1);
                });

                // This means blockheight == 0 for IBD.
                let (blockheight, _) = db_tip(&db_path).unwrap_or_else(|e| {
                    log::error!("Getting tip from db: {:?}", e);
                    process::exit(1);
                });

                response_tx
                    .send((network.to_string(), blockheight, progress))
                    // TODO: a macro for the unwrap_or_else boilerplate..
                    .unwrap_or_else(|e| {
                        log::error!("Sending 'getinfo' result to RPC thread: {:?}", e);
                        process::exit(1);
                    });
            }
            RpcMessageIn::ListVaults((status, txids), response_tx) => {
                log::trace!("Got listvaults from RPC thread");

                let mut resp = Vec::<(u64, String, String, u32)>::new();
                for (ref outpoint, ref vault) in revaultd.read().unwrap().vaults.iter() {
                    if let Some(status) = status {
                        if vault.status != status {
                            continue;
                        }
                    }

                    if let Some(ref txids) = &txids {
                        if !txids.contains(&outpoint.txid) {
                            continue;
                        }
                    }

                    resp.push((
                        vault.txo.value,
                        vault.status.to_string(),
                        outpoint.txid.to_string(),
                        outpoint.vout,
                    ));
                }

                response_tx.send(resp).unwrap_or_else(|e| {
                    log::error!("Sending 'listvaults' result to RPC thread: {:?}", e);
                    process::exit(1);
                });
            }
        }
    }
}

// This creates the log file automagically if it doesn't exist, and logs on stdout
// if None is given
fn setup_logger(
    log_file: Option<&str>,
    log_level: log::LevelFilter,
) -> Result<(), fern::InitError> {
    let dispatcher = fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{}[{}][{}] {}",
                chrono::Local::now().format("[%Y-%m-%d][%H:%M:%S]"),
                record.target(),
                record.level(),
                message
            ))
        })
        .level(log_level);

    if let Some(log_file) = log_file {
        dispatcher.chain(fern::log_file(log_file)?).apply()?;
    } else {
        dispatcher.chain(std::io::stdout()).apply()?;
    }

    Ok(())
}

fn main() {
    let args = env::args().collect();
    let conf_file = parse_args(args);

    let config = Config::from_file(conf_file).unwrap_or_else(|e| {
        eprintln!("Error parsing config: {}", e);
        process::exit(1);
    });
    let log_level = if let Some(ref level) = &config.log_level {
        log::LevelFilter::from_str(level.as_str()).unwrap_or_else(|e| {
            eprintln!("Invalid log level: {}", e);
            process::exit(1);
        })
    } else {
        log::LevelFilter::Info
    };
    // FIXME: should probably be from_db(), would allow us to not use Option members
    let revaultd = RevaultD::from_config(config).unwrap_or_else(|e| {
        eprintln!("Error creating global state: {}", e);
        process::exit(1);
    });

    let log_file = revaultd.log_file();
    let log_output = if revaultd.daemon {
        Some(log_file.to_str().expect("Valid unicode"))
    } else {
        None
    };
    setup_logger(log_output, log_level).unwrap_or_else(|e| {
        eprintln!("Error setting up logger: {}", e);
        process::exit(1);
    });

    if revaultd.daemon {
        let mut daemon = Daemonize::default();
        // TODO: Make this configurable for inits
        daemon.pid_file = Some(revaultd.pid_file());
        daemon.doit().unwrap_or_else(|e| {
            eprintln!("Error daemonizing: {}", e);
            process::exit(1);
        });
        println!("Started revaultd daemon");
    }

    daemon_main(revaultd);
}
