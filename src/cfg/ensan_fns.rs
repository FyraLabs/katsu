#[ensan_proc_macro::ensan_internal_fn_mod(katsu_ensan_fns)]
pub mod katsu_fns {
	use hcl::eval::FuncArgs;
	type FnRes = Result<hcl::Value, String>;

	#[ensan_fn(Any)]
	pub fn output_file(args: FuncArgs) -> FnRes {
		todo!()
	}
}
