// This file is part of Substrate.

// Copyright (C) 2022 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use sc_cli::Result;
use sc_service::Configuration;

use log::info;
use serde::Serialize;
use std::{env, fs, path::PathBuf};

use super::{cmd::StorageParams, record::Stats};

static VERSION: &'static str = env!("CARGO_PKG_VERSION");
static TEMPLATE: &str = include_str!("./weights.hbs");

/// Data consumed by Handlebar to fill out the `weights.hbs` template.
#[derive(Serialize, Default, Debug, Clone)]
pub(crate) struct TemplateData {
	/// Name of the database used.
	db_name: String,
	/// Name of the runtime. Taken from the chain spec.
	runtime_name: String,
	/// Version of the benchmarking CLI used.
	version: String,
	/// Date that the template was filled out.
	date: String,
	/// Command line arguments that were passed to the CLI.
	args: Vec<String>,
	/// Storage params of the executed command.
	params: StorageParams,
	/// The weight for one `read`.
	read_weight: u64,
	/// The weight for one `write`.
	write_weight: u64,
	/// Stats about a `read` benchmark. Contains *time* and *value size* stats.
	/// The *value size* stats are currently not used in the template.
	read: Option<(Stats, Stats)>,
	/// Stats about a `write` benchmark. Contains *time* and *value size* stats.
	/// The *value size* stats are currently not used in the template.
	write: Option<(Stats, Stats)>,
}

impl TemplateData {
	/// Returns a new [`Self`] from the given configuration.
	pub fn new(cfg: &Configuration, params: &StorageParams) -> Self {
		TemplateData {
			db_name: format!("{}", cfg.database),
			runtime_name: cfg.chain_spec.name().into(),
			version: VERSION.into(),
			date: chrono::Utc::now().format("%Y-%m-%d (Y/M/D)").to_string(),
			args: env::args().collect::<Vec<String>>(),
			params: params.clone(),
			..Default::default()
		}
	}

	/// Sets the stats and calculates the final weights.
	pub fn set_stats(
		&mut self,
		read: Option<(Stats, Stats)>,
		write: Option<(Stats, Stats)>,
	) -> Result<()> {
		if let Some(read) = read {
			self.read_weight = calc_weight(&read.0, &self.params)?;
			self.read = Some(read);
		}
		if let Some(write) = write {
			self.write_weight = calc_weight(&write.0, &self.params)?;
			self.write = Some(write);
		}
		Ok(())
	}

	/// Filles out the `weights.hbs` HBS template with its own data.
	/// Writes the result to `path` which can be a directory or file.
	pub fn write(&self, path: &str) -> Result<()> {
		let mut handlebars = handlebars::Handlebars::new();
		// Format large integers with underscore.
		handlebars.register_helper("underscore", Box::new(crate::writer::UnderscoreHelper));
		// Don't HTML escape any characters.
		handlebars.register_escape_fn(|s| -> String { s.to_string() });

		let out_path = self.build_path(path);
		let mut fd = fs::File::create(&out_path)?;
		info!("Writing weights to {:?}", fs::canonicalize(&out_path)?);
		handlebars
			.render_template_to_write(&TEMPLATE, &self, &mut fd)
			.map_err(|e| format!("HBS template write: {:?}", e).into())
	}

	/// Builds a path for the weight file.
	fn build_path(&self, weight_out: &str) -> PathBuf {
		let mut path = PathBuf::from(weight_out);
		if path.is_dir() {
			path.push(format!("{}_weights.rs", self.db_name.to_lowercase()));
			path.set_extension("rs");
		}
		path
	}
}

/// Calculates the final weight by multiplying the selected metric with
/// `mul` and adding `add`.
/// Does not use safe casts and can overflow.
fn calc_weight(stat: &Stats, params: &StorageParams) -> Result<u64> {
	if params.weight_mul.is_sign_negative() || !params.weight_mul.is_normal() {
		return Err("invalid floating number for `weight_mul`".into())
	}
	let s = stat.select(params.weight_metric) as f64;
	let w = s.mul_add(params.weight_mul, params.weight_add as f64).ceil();
	Ok(w as u64) // No safe cast here since there is no `From<f64>` for `u64`.
}
