use crate::internal_prelude::*;

const REPOSITORY_URL: &str = "https://github.com/paritytech/polkadot-sdk";

pub(crate) fn checkout(branch: &str) -> Result<Oid> {
    let directory = SDK_DIRECTORY.as_path();
    let mut fetch_options = FetchOptions::new();
    fetch_options.depth(1).download_tags(AutotagOption::None);
    if !directory.try_exists()? {
        let repository = RepoBuilder::new()
            .branch(branch)
            .fetch_options(fetch_options)
            .remote_create(|repository, name, url| {
                repository.remote_with_fetch(
                    name,
                    url,
                    &format!("+refs/heads/{branch}:refs/remotes/{name}/{branch}"),
                )
            })
            .clone(REPOSITORY_URL, directory)
            .with_context(|| format!("Failed to clone polkadot-sdk branch {branch}"))?;
        return Ok(repository.head()?.peel_to_commit()?.id());
    }

    let repository = Repository::open(directory).context("Failed to open SDK checkout")?;
    let remote_ref = format!("refs/remotes/origin/{branch}");
    repository
        .remote_anonymous(REPOSITORY_URL)?
        .fetch(
            &[format!("+refs/heads/{branch}:{remote_ref}")],
            Some(&mut fetch_options),
            None,
        )
        .with_context(|| format!("Failed to fetch polkadot-sdk branch {branch}"))?;
    let commit = repository.find_reference(&remote_ref)?.peel_to_commit()?;
    repository.checkout_tree(commit.as_object(), Some(CheckoutBuilder::new().force()))?;

    let local_ref = format!("refs/heads/{branch}");
    let head = repository.head()?;
    if head.target() != Some(commit.id()) || head.name()? != local_ref.as_str() {
        repository.reference(&local_ref, commit.id(), true, "Update SDK checkout")?;
        repository.set_head(&local_ref)?;
    }
    Ok(commit.id())
}
