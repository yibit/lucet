#[macro_use]
extern crate clap;

use clap::Arg;
use lucet_runtime::{self, DlModule, Limits, MmapRegion, Module, Region};
use lucet_wasi::{hostcalls, WasiCtxBuilder};
use std::fs::File;
use std::sync::Arc;

struct Config<'a> {
    lucet_module: &'a str,
    guest_args: Vec<&'a str>,
    entrypoint: &'a str,
    preopen_dirs: Vec<(File, &'a str)>,
}

fn main() {
    // No-ops, but makes sure the linker doesn't throw away parts
    // of the runtime:
    lucet_runtime::lucet_internal_ensure_linked();
    hostcalls::ensure_linked();

    let matches = app_from_crate!()
        .arg(
            Arg::with_name("entrypoint")
                .long("entrypoint")
                .takes_value(true)
                .default_value("_start")
                .help("Entrypoint to run within the WASI module"),
        )
        .arg(
            Arg::with_name("lucet_module")
                .required(true)
                .help("Path to the `lucetc`-compiled WASI module"),
        )
        .arg(
            Arg::with_name("preopen_dirs")
                .required(false)
                .long("dir")
                .takes_value(true)
                .multiple(true)
                .help("Directories to provide to the WASI guest")
                .long_help(
                    "Directories on the host can be provided to the WASI guest as part of a \
                     virtual filesystem. Each directory is specified as a \
                     `host_path:guest_path`, where `guest_path` specifies the  path that will \
                     correspond to `host_path` for calls like `fopen` in the guest.\
                     \
                     For example, `--dir /home/host_user/wasi_sandbox:/sandbox` will make \
                     `/home/host_user/wasi_sandbox` available within the guest as `/sandbox`.\
                     \
                     Guests will be able to access any files and directories under the \
                     `host_path`, but will be unable to access other parts of the host \
                     filesystem through relative paths (e.g., `/sandbox/../some_other_file`) \
                     or through symlinks.",
                ),
        )
        .arg(
            Arg::with_name("guest_args")
                .required(false)
                .multiple(true)
                .help("Arguments to the WASI `main` function"),
        )
        .get_matches();

    let entrypoint = matches.value_of("entrypoint").unwrap();
    let lucet_module = matches.value_of("lucet_module").unwrap();
    let preopen_dirs = matches
        .values_of("preopen_dirs")
        .map(|vals| {
            vals.map(|preopen_dir| {
                if let [host_path, guest_path] =
                    preopen_dir.split(':').collect::<Vec<&str>>().as_slice()
                {
                    let host_dir = File::open(host_path).unwrap();
                    (host_dir, *guest_path)
                } else {
                    println!("Invalid directory specification: {}", preopen_dir);
                    println!("{}", matches.usage());
                    std::process::exit(1);
                }
            })
            .collect()
        })
        .unwrap_or(vec![]);
    let guest_args = matches
        .values_of("guest_args")
        .map(|vals| vals.collect())
        .unwrap_or(vec![]);
    let config = Config {
        lucet_module,
        guest_args,
        entrypoint,
        preopen_dirs,
    };
    run(config)
}

fn run(config: Config) {
    lucet_wasi::hostcalls::ensure_linked();
    let exitcode = {
        // doing all of this in a block makes sure everything gets dropped before exiting
        let region = MmapRegion::create(1, &Limits::default()).expect("region can be created");
        let module = DlModule::load(&config.lucet_module).expect("module can be loaded");

        // put the path to the module on the front for argv[0]
        let args = std::iter::once(config.lucet_module)
            .chain(config.guest_args.into_iter())
            .collect::<Vec<&str>>();
        let mut ctx = WasiCtxBuilder::new().args(&args).inherit_env();
        for (dir, guest_path) in config.preopen_dirs {
            ctx = ctx.preopened_dir(dir, guest_path);
        }
        let mut inst = region
            .new_instance_builder(module as Arc<dyn Module>)
            .with_embed_ctx(ctx.build().expect("WASI ctx can be created"))
            .build()
            .expect("instance can be created");

        match inst.run(config.entrypoint.as_bytes(), &[]) {
            // normal termination implies 0 exit code
            Ok(_) => 0,
            Err(lucet_runtime::Error::RuntimeTerminated(
                lucet_runtime::TerminationDetails::Provided(any),
            )) => *any
                .downcast_ref::<lucet_wasi::host::__wasi_exitcode_t>()
                .expect("termination yields an exitcode"),
            Err(e) => panic!("lucet-wasi runtime error: {}", e),
        }
    };
    std::process::exit(exitcode as i32);
}
