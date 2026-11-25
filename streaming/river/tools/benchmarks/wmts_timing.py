"""Keep historical benchmark totals comparable across WMTS header versions."""

def render_elapsed_ms(headers):
    values = {key.lower(): value for key, value in headers.items()}
    if "server-timing" not in values:
        return float(values["x-river-server-ms"])
    # The legacy timer started after asset resolution.
    return sum(float(duration) for name, duration in
               (metric.strip().split(";dur=") for metric in values["server-timing"].split(","))
               if name not in ("locate_image", "resolve"))
