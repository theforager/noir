use acvm::acir::circuit::opcodes::BlackBoxFuncCall;
use acvm::acir::circuit::Opcode;
use acvm::Language;

use nargo::artifacts::program::PreprocessedProgram;
use nargo::package::Package;
use nargo::prepare_package;
use nargo::workspace::Workspace;
use nargo_toml::{get_package_manifest, resolve_workspace_from_toml, PackageSelection};
use noirc_driver::compile_no_check;
use noirc_driver::CompileOptions;
use noirc_driver::CompiledProgram;
use noirc_driver::NOIR_ARTIFACT_VERSION_STRING;
use noirc_frontend::graph::CrateName;

use clap::Args;

use crate::backends::Backend;
use crate::errors::CliError;

use super::check_cmd::check_crate_and_report_errors;
use super::compile_cmd::save_program;

use super::fs::program::save_program_to_file;
use super::NargoConfig;

// TODO(#1388): pull this from backend.
const BACKEND_IDENTIFIER: &str = "acvm-backend-barretenberg";

/// Compile the program and its secret execution trace into ACIR format
#[derive(Debug, Clone, Args)]
pub(crate) struct ExportCommand {
    /// The name of the package to compile
    #[clap(long, conflicts_with = "workspace")]
    package: Option<CrateName>,

    /// Compile all packages in the workspace
    #[clap(long, conflicts_with = "package")]
    workspace: bool,

    #[clap(flatten)]
    compile_options: CompileOptions,
}

pub(crate) fn run(
    backend: &Backend,
    args: ExportCommand,
    config: NargoConfig,
) -> Result<(), CliError> {
    let toml_path = get_package_manifest(&config.program_dir)?;
    let default_selection =
        if args.workspace { PackageSelection::All } else { PackageSelection::DefaultOrAll };
    let selection = args.package.map_or(default_selection, PackageSelection::Selected);

    let workspace = resolve_workspace_from_toml(
        &toml_path,
        selection,
        Some(NOIR_ARTIFACT_VERSION_STRING.to_owned()),
    )?;
    let circuit_dir = workspace.target_directory_path();

    let library_packages: Vec<_> =
        workspace.into_iter().filter(|package| package.is_library()).collect();

    let (np_language, opcode_support) = backend.get_backend_info()?;
    let is_opcode_supported = |opcode: &_| opcode_support.is_opcode_supported(opcode);

    compile_program(
        &workspace,
        &library_packages[0],
        &args.compile_options,
        np_language,
        &is_opcode_supported,
    )?;

    Ok(())
}

fn compile_program(
    workspace: &Workspace,
    package: &Package,
    compile_options: &CompileOptions,
    np_language: Language,
    is_opcode_supported: &impl Fn(&Opcode) -> bool,
) -> Result<(), CliError> {
    let (mut context, crate_id) =
        prepare_package(package, Box::new(|path| std::fs::read_to_string(path)));
    check_crate_and_report_errors(
        &mut context,
        crate_id,
        compile_options.deny_warnings,
        compile_options.silence_warnings,
    )?;

    let exported_functions = context.get_all_exported_functions_in_crate(&crate_id);

    // TODO: we say that pedersen hashing is supported by all backends for now
    let is_opcode_supported_pedersen_hash = |opcode: &Opcode| -> bool {
        if let Opcode::BlackBoxFuncCall(BlackBoxFuncCall::PedersenHash { .. }) = opcode {
            true
        } else {
            is_opcode_supported(opcode)
        }
    };

    let exported_programs: Vec<_> = exported_functions
        .into_iter()
        .map(|(function_name, function_id)| -> (String, CompiledProgram) {
            let program = compile_no_check(&context, compile_options, function_id, None, false)
                .expect("heyooo");

            // Apply backend specific optimizations.
            let optimized_program = nargo::ops::optimize_program(
                program,
                np_language,
                &is_opcode_supported_pedersen_hash,
            )
            .expect("Backend does not support an opcode that is in the IR");

            (function_name, optimized_program)
        })
        .collect();

    for (function_name, program) in exported_programs {
        let preprocessed_program = PreprocessedProgram {
            hash: program.hash,
            backend: String::from(BACKEND_IDENTIFIER),
            abi: program.abi,
            noir_version: program.noir_version,
            bytecode: program.circuit,
        };

        save_program_to_file(
            &preprocessed_program,
            &function_name.parse().unwrap(),
            workspace.target_directory_path(),
        );
    }
    Ok(())
}
