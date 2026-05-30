use crate::internal_prelude::*;

static ALREADY_ALLOCATED: LazyLock<Arc<StdMutex<Vec<u16>>>> = LazyLock::new(Default::default);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct PortAllocator;

impl PortAllocator {
    pub fn allocate_port() -> anyhow::Result<u16> {
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
            return Ok(port);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_distinct_ports_when_called_repeatedly() -> anyhow::Result<()> {
        // Act
        let first_port = PortAllocator::allocate_port()?;
        let second_port = PortAllocator::allocate_port()?;

        // Assert
        assert_ne!(first_port, second_port);
        Ok(())
    }

    #[test]
    fn returns_port_that_can_be_bound_on_localhost() -> anyhow::Result<()> {
        // Act
        let port = PortAllocator::allocate_port()?;
        let listener = TcpListener::bind(("127.0.0.1", port))
            .context("Failed to bind allocated localhost port")?;

        // Assert
        assert_eq!(listener.local_addr()?.port(), port);
        Ok(())
    }
}
