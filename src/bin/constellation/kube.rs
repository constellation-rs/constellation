#![allow(clippy::module_name_repetitions)]

use ::kube::{
	api::{Api, ListParams, PatchParams}, client::APIClient, config
};
use serde_json::json;
use std::{
	collections::HashMap, env, fs::read_to_string, net::{IpAddr, SocketAddr}, thread
};

use super::master;
use constellation_internal::{abort_on_unwind, Pid, PidInternal};

pub fn kube_master(
	master_bind: SocketAddr, fabric_port: u16, bridge_bind: SocketAddr, mem: u64, cpu: u32,
	replicas: u32,
) {
	let namespace =
		read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace").unwrap();

	let config = config::incluster_config().expect("failed to load in-cluster kubeconfig");
	let client = APIClient::new(config);

	let jobs = Api::v1ReplicaSet(client.clone()).within(&namespace); //.group("extensions").version("v1beta1");

	let replicas = replicas - 1; // one is this master

	let fs = json!({
		"spec": { "replicas": replicas }
	});
	let _ = jobs
		.patch_scale(
			"constellation",
			&PatchParams::default(),
			serde_json::to_vec(&fs).unwrap(),
		)
		.unwrap();

	let pods = Api::v1Pod(client).within(&namespace);

	let ips = loop {
		let pods = pods
			.list(&ListParams {
				label_selector: Some(format!("{}={}", "constellation", "node")),
				..ListParams::default()
			})
			.expect("failed to list pods")
			.items;
		let ips: Vec<IpAddr> = pods
			.into_iter()
			.filter_map(|pod| Some(pod.status?.pod_ip?.parse().unwrap()))
			.collect();
		if ips.len() == replicas as usize {
			break ips;
		}
		std::thread::sleep(std::time::Duration::from_secs(2));
	};

	let _ = thread::Builder::new()
		.name(String::from("master"))
		.spawn(abort_on_unwind(move || {
			std::thread::sleep(std::time::Duration::from_secs(10));

			let master_addr = SocketAddr::new(
				env::var("CONSTELLATION_IP").unwrap().parse().unwrap(),
				master_bind.port(),
			);

			let mut nodes = ips
				.into_iter()
				.map(|ip| {
					let fabric = SocketAddr::new(ip, master_bind.port());
					let bridge = None;
					(fabric, (bridge, mem, cpu))
				})
				.collect::<HashMap<_, _>>(); // TODO: error on clash
			let _ = nodes.insert(
				SocketAddr::new(master_addr.ip(), fabric_port),
				(Some(bridge_bind), mem, cpu),
			);

			master::run(
				SocketAddr::new(master_bind.ip(), master_addr.port()),
				Pid::new(master_addr.ip(), master_addr.port()),
				nodes,
			)
		}))
		.unwrap();
}
