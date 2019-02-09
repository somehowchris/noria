use fnv::FnvHashMap;
use prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Serialize, Deserialize)]
struct EgressTx {
    node: NodeIndex,
    local: LocalNodeIndex,
    dest: ReplicaAddr,
}

#[derive(Serialize, Deserialize)]
pub struct Egress {
    txs: Vec<EgressTx>,
    tags: HashMap<Tag, NodeIndex>,
}

impl Clone for Egress {
    fn clone(&self) -> Self {
        assert!(self.txs.is_empty());

        Self {
            txs: Vec::new(),
            tags: self.tags.clone(),
        }
    }
}

impl Default for Egress {
    fn default() -> Self {
        Self {
            tags: Default::default(),
            txs: Default::default(),
        }
    }
}

impl Egress {
    pub fn get_tx_nodes(&self) -> HashSet<NodeIndex> {
        self.txs.iter().map(|tx| tx.node).collect()
    }

    pub fn add_tx(&mut self, dst_g: NodeIndex, dst_l: LocalNodeIndex, addr: ReplicaAddr) {
        self.txs.push(EgressTx {
            node: dst_g,
            local: dst_l,
            dest: addr,
        });
    }

    pub fn add_tag(&mut self, tag: Tag, dst: NodeIndex) {
        self.tags.insert(tag, dst);
    }

    /// TODO(ygina): a hack
    /// is there state about replay paths somewhere else? like in materialization/plan.rs?
    ///
    /// Replace the old single egress tx with this new tx, also replacing the replay path
    /// the old tx was previously a part of. Makes a lot of assumptions here, like only having
    /// one outgoing connection, or that the paths are the same.
    pub fn replace_tx(&mut self, dst_g: NodeIndex, dst_l: LocalNodeIndex, addr: ReplicaAddr) {
        self.txs.clear();
        self.add_tx(dst_g, dst_l, addr);
        assert_eq!(self.tags.len(), 1);

        // update the replay path
        let tag = self.tags.keys().next().unwrap();
        self.tags.insert(*tag, dst_g);
    }

    pub fn process(
        &mut self,
        m: &mut Option<Box<Packet>>,
        shard: usize,
        output: &mut FnvHashMap<ReplicaAddr, VecDeque<Box<Packet>>>,
        to_nodes: &HashSet<NodeIndex>,
    ) {
        let &mut Self {
            ref mut txs,
            ref tags,
        } = self;

        // send any queued updates to all external children
        assert!(txs.len() > 0);
        let txn = txs.len() - 1;

        // we need to find the ingress node following this egress according to the path
        // with replay.tag, and then forward this message only on the channel corresponding
        // to that ingress node.
        let replay_to = m.as_ref().unwrap().tag().map(|tag| {
            tags.get(&tag)
                .map(|n| *n)
                .expect("egress node told about replay message, but not on replay path")
        });

        for (txi, ref mut tx) in txs.iter_mut().enumerate() {
            if !to_nodes.contains(&tx.node) {
                continue;
            }

            let mut take = txi == txn;
            if let Some(replay_to) = replay_to.as_ref() {
                if *replay_to == tx.node {
                    take = true;
                } else {
                    continue;
                }
            }

            // Avoid cloning if this is last send
            let mut m = if take {
                m.take().unwrap()
            } else {
                // we know this is a data (not a replay)
                // because, a replay will force a take
                m.as_ref().map(|m| box m.clone_data()).unwrap()
            };

            // src is usually ignored and overwritten by ingress
            // *except* if the ingress is marked as a shard merger
            // in which case it wants to know about the shard
            m.link_mut().src = unsafe { LocalNodeIndex::make(shard as u32) };
            m.link_mut().dst = tx.local;

            output.entry(tx.dest).or_default().push_back(m);
            if take {
                break;
            }
        }
    }
}
