use crate::internal_prelude::*;

pub(crate) mod data;

pub(crate) fn generate(
    report_path: impl AsRef<Path>,
    output_path: impl AsRef<Path>,
    template_path: Option<&Path>,
) -> Result<()> {
    let report_path = report_path.as_ref().canonicalize()?;
    let output_path = output_path.as_ref();
    let data_path = output_path.with_extension("data.json.gz");
    for path in [output_path, data_path.as_path()]
        .into_iter()
        .filter(|path| path.exists())
    {
        let output = path.canonicalize()?;
        ensure!(output != report_path, "Output would overwrite the report");
        if let Some(template_path) = template_path {
            ensure!(
                output != template_path.canonicalize()?,
                "Output would overwrite the HTML template"
            );
        }
    }
    let template = match template_path {
        Some(path) => read_to_string(path).context("Failed to read profiling template")?,
        None => include_str!("../../../assets/profiling-report.html").to_owned(),
    };
    const MARKER: &str = "<!-- PROFILING_DATA -->";
    ensure!(
        template.matches(MARKER).count() == 1,
        "Expected one profiling data marker in the HTML template"
    );
    let reader = BufReader::new(File::open(&report_path)?);
    let data = if report_path
        .extension()
        .is_some_and(|extension| extension == "gz")
    {
        serde_json::from_reader::<_, ProfilingData>(GzDecoder::new(reader))
    } else {
        serde_json::from_reader::<_, ProfilingData>(reader)
    }
    .context("Failed to read profiling report")?;
    ensure!(
        data.workloads
            .iter()
            .any(|workload| !workload.transactions.is_empty()),
        "No repeated transactions found. The report must contain repeat_path metadata."
    );
    let data_file_name = data_path
        .file_name()
        .and_then(|name| name.to_str())
        .context("Output filename is not valid UTF-8")?;
    let mut encoder = GzEncoder::new(
        BufWriter::new(File::create(&data_path)?),
        Compression::default(),
    );
    serde_json::to_writer(&mut encoder, &data).context("Failed to write profiling data")?;
    encoder.finish()?.into_inner()?;
    let report_file = serde_json::to_string(data_file_name)?.replace('<', "\\u003c");
    let html = template.replacen(
        MARKER,
        &format!("<script>const REPORT_FILE={report_file};</script>"),
        1,
    );
    std::fs::write(output_path, html).context("Failed to write profiling HTML")?;
    println!(
        "Profiling visualization written to {}",
        output_path.display()
    );
    Ok(())
}
