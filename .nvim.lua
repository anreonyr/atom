vim.g.rustaceanvim = {
	server = {
		default_settings = {
			["rust-analyzer"] = {
				cargo = {
					target = "riscv64gc-unknown-none-elf",
					allTargets = false,
				},
			},
		},
	},
}
