export function renderElapsedMs(headers) {
  const stages = headers.get("server-timing");
  if (!stages) return Number(headers.get("x-river-server-ms"));
  return stages.split(",").reduce((sum, metric) => {
    const [name, duration] = metric.trim().split(";dur=");
    return (name === "resolve" || name === "locate_image") ? sum : sum + Number(duration);
  }, 0);
}
