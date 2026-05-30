use crate::internal_prelude::*;

static ALREADY_ALLOCATED: LazyLock<Arc<StdMutex<Vec<u16>>>> = LazyLock::new(Default::default);

/// An allocated port. This is a guard which allows for the port to not be used by another process.
#[derive(Debug)]
pub struct AllocatedPort(TcpListener);

impl AllocatedPort {
    pub fn allocate() -> anyhow::Result<Self> {
        loop {
            let listener = TcpListener::bind("127.0.0.1:0")
                .context("Failed to bind to an available localhost port")?;
            let port = listener
                .local_addr()
                .context("Failed to read allocated localhost port")?
                .port();
            let mut already_allocated = ALREADY_ALLOCATED
                .lock()
                .map_err(|_| anyhow::anyhow!("Allocated port mutex has been poisoned"))?;

            if already_allocated.contains(&port) {
                continue;
            }

            already_allocated.push(port);
            return Ok(Self(listener));
        }
    }

    pub fn value(self) -> u16 {
        self.0
            .local_addr()
            .expect("qed; allocated listener is bound to a local address")
            .port()
    }

    fn port(&self) -> u16 {
        self.0
            .local_addr()
            .expect("qed; allocated listener is bound to a local address")
            .port()
    }
}

impl std::fmt::Display for AllocatedPort {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.port().fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_distinct_ports_when_called_repeatedly() -> anyhow::Result<()> {
        let first_port = AllocatedPort::allocate()?;
        let second_port = AllocatedPort::allocate()?;
        let first_port = first_port.value();
        let second_port = second_port.value();

        assert_ne!(first_port, second_port);
        Ok(())
    }

    #[test]
    fn does_not_reuse_consumed_port_reserved_in_static() -> anyhow::Result<()> {
        let consumed_port = AllocatedPort::allocate()?.value();
        let next_port = AllocatedPort::allocate()?.value();

        assert_ne!(consumed_port, next_port);
        Ok(())
    }
}
