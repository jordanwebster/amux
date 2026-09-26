//! A store held in memory, for tests and replay. A write runs against a
//! copy that replaces the data only when the whole transaction succeeds.

use std::collections::{BTreeMap, HashMap};

use crate::{
    AgentKey, AgentRow, Backend, Delivery, Item, Marker, Notification, StoreError, Tables,
    item_bytes,
};

#[derive(Clone, Debug, Default)]
struct Data {
    agents: BTreeMap<AgentKey, AgentRow>,
    /// Per agent: key to item, and order to key (one key per order).
    items: BTreeMap<AgentKey, BTreeMap<String, Item>>,
    orders: BTreeMap<AgentKey, BTreeMap<u64, String>>,
    deliveries: BTreeMap<(Vec<u8>, u32, i32, u64), Delivery>,
    notifications: BTreeMap<(Vec<u8>, u64), Notification>,
    hosts: BTreeMap<Vec<u8>, u64>,
}

#[derive(Debug, Default)]
pub struct InMemory {
    own_host: Vec<u8>,
    data: Data,
    markers: HashMap<AgentKey, Marker>,
}

impl InMemory {
    pub fn new(own_host: impl Into<Vec<u8>>) -> Self {
        Self {
            own_host: own_host.into(),
            ..Self::default()
        }
    }
}

impl Backend for InMemory {
    fn own_host(&self) -> &[u8] {
        &self.own_host
    }

    fn read<R>(
        &self,
        f: impl FnOnce(&dyn Tables) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        f(&self.data)
    }

    fn write<R>(
        &mut self,
        f: impl FnOnce(&mut dyn Tables) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        let mut draft = self.data.clone();
        let result = f(&mut draft)?;
        self.data = draft;
        Ok(result)
    }

    fn markers(&self) -> &HashMap<AgentKey, Marker> {
        &self.markers
    }

    fn markers_mut(&mut self) -> &mut HashMap<AgentKey, Marker> {
        &mut self.markers
    }
}

impl Tables for Data {
    fn agent(&self, agent: &AgentKey) -> Result<Option<AgentRow>, StoreError> {
        Ok(self.agents.get(agent).cloned())
    }

    fn put_agent(&mut self, row: &AgentRow) -> Result<(), StoreError> {
        self.agents.insert(row.agent.clone(), row.clone());
        Ok(())
    }

    fn remove_agent(&mut self, agent: &AgentKey) -> Result<(), StoreError> {
        self.agents.remove(agent);
        self.items.remove(agent);
        self.orders.remove(agent);
        self.deliveries
            .retain(|(child, ..), _| *child != agent.agent);
        self.notifications
            .retain(|(agent_id, _), _| *agent_id != agent.agent);
        Ok(())
    }

    fn agents_of_host(&self, host: &[u8]) -> Result<Vec<AgentKey>, StoreError> {
        Ok(self
            .agents
            .keys()
            .filter(|agent| agent.host == host)
            .cloned()
            .collect())
    }

    fn item(&self, agent: &AgentKey, key: &str) -> Result<Option<Item>, StoreError> {
        Ok(self
            .items
            .get(agent)
            .and_then(|items| items.get(key))
            .cloned())
    }

    fn put_item(&mut self, agent: &AgentKey, item: &Item) -> Result<(), StoreError> {
        let orders = self.orders.entry(agent.clone()).or_default();
        if let Some(key) = orders.get(&item.order)
            && *key != item.key
        {
            return Err(StoreError::Corrupt(format!(
                "order {} already belongs to key {key}",
                item.order
            )));
        }
        let items = self.items.entry(agent.clone()).or_default();
        if let Some(previous) = items.get(&item.key) {
            orders.remove(&previous.order);
        }
        orders.insert(item.order, item.key.clone());
        items.insert(item.key.clone(), item.clone());
        Ok(())
    }

    fn max_order(&self, agent: &AgentKey) -> Result<Option<u64>, StoreError> {
        Ok(self
            .orders
            .get(agent)
            .and_then(|orders| orders.keys().next_back().copied()))
    }

    fn items_desc(
        &self,
        agent: &AgentKey,
        below: Option<u64>,
        min_order: u64,
        limit: u32,
    ) -> Result<Vec<Item>, StoreError> {
        let (Some(orders), Some(items)) = (self.orders.get(agent), self.items.get(agent)) else {
            return Ok(Vec::new());
        };
        let upper = below.unwrap_or(u64::MAX);
        if upper <= min_order {
            return Ok(Vec::new());
        }
        Ok(orders
            .range(min_order..upper)
            .rev()
            .take(limit as usize)
            .map(|(_, key)| items[key].clone())
            .collect())
    }

    fn put_delivery(&mut self, delivery: &Delivery) -> Result<(), StoreError> {
        self.deliveries.insert(
            (
                delivery.child_id.clone(),
                delivery.incarnation,
                delivery.kind,
                delivery.turn_id,
            ),
            delivery.clone(),
        );
        Ok(())
    }

    fn deliveries(&self) -> Result<Vec<Delivery>, StoreError> {
        Ok(self.deliveries.values().cloned().collect())
    }

    fn remove_delivery(
        &mut self,
        child_id: &[u8],
        incarnation: u32,
        kind: i32,
        turn_id: u64,
    ) -> Result<(), StoreError> {
        self.deliveries
            .remove(&(child_id.to_vec(), incarnation, kind, turn_id));
        Ok(())
    }

    fn put_notification(&mut self, notification: &Notification) -> Result<(), StoreError> {
        self.notifications.insert(
            (notification.agent_id.clone(), notification.revision),
            notification.clone(),
        );
        Ok(())
    }

    fn notifications(&self) -> Result<Vec<Notification>, StoreError> {
        Ok(self.notifications.values().cloned().collect())
    }

    fn remove_notifications(&mut self, agent_id: &[u8]) -> Result<(), StoreError> {
        self.notifications
            .retain(|(agent, _), _| agent.as_slice() != agent_id);
        Ok(())
    }

    fn host_generation(&self, host: &[u8]) -> Result<Option<u64>, StoreError> {
        Ok(self.hosts.get(host).copied())
    }

    fn set_host_generation(&mut self, host: &[u8], generation: u64) -> Result<(), StoreError> {
        self.hosts.insert(host.to_vec(), generation);
        Ok(())
    }

    fn agents(&self) -> Result<Vec<AgentRow>, StoreError> {
        Ok(self.agents.values().cloned().collect())
    }

    fn agent_bytes(&self, agent: &AgentKey) -> Result<(u64, u64), StoreError> {
        Ok(self.items.get(agent).map_or((0, 0), |items| {
            (items.values().map(item_bytes).sum(), items.len() as u64)
        }))
    }

    fn oldest_items(&self, agent: &AgentKey, limit: u32) -> Result<Vec<(u64, u64)>, StoreError> {
        let (Some(orders), Some(items)) = (self.orders.get(agent), self.items.get(agent)) else {
            return Ok(Vec::new());
        };
        Ok(orders
            .iter()
            .take(limit as usize)
            .map(|(order, key)| (*order, item_bytes(&items[key])))
            .collect())
    }

    fn remove_items_below(&mut self, agent: &AgentKey, order: u64) -> Result<(), StoreError> {
        if let (Some(orders), Some(items)) = (self.orders.get_mut(agent), self.items.get_mut(agent))
        {
            let kept = orders.split_off(&order);
            for key in orders.values() {
                items.remove(key);
            }
            *orders = kept;
        }
        Ok(())
    }
}
