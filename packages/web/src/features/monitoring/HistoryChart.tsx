import { formatMetric as number, type History } from "./metrics";

const colors = [
  "#2563eb",
  "#0d9488",
  "#d97706",
  "#9333ea",
  "#dc2626",
  "#475569",
  "#db2777",
  "#65a30d",
];

/** Draws finite samples on their timestamps, breaking paths at missing/NaN values. */
export function HistoryChart({
  data,
  title,
  unit,
}: {
  data: History;
  title: string;
  unit: string;
}) {
  const times = data.result.flatMap((series) =>
    series.values.map(([time]) => time),
  );
  const finite = data.result
    .flatMap((series) => series.values.map(([, val]) => Number(val)))
    .filter(Number.isFinite);
  if (!times.length || !finite.length)
    return (
      <p className="monitoring-empty">
        此时间范围尚无有效采样。首次启动至少需要两次抓取才能计算速率。
      </p>
    );
  const first = Math.min(...times);
  const last = Math.max(...times);
  const peak = Math.max(...finite);
  const high = peak > 0 ? peak * 1.1 : 1;
  return (
    <>
      <h2>
        {title} <small>({unit})</small>
      </h2>
      <svg
        className="monitoring-chart"
        viewBox="0 0 960 300"
        role="img"
        aria-label={`${title}历史曲线，单位${unit}，下方表格提供各序列最新值`}
      >
        {[0, 0.5, 1].map((part) => (
          <g key={part}>
            <line
              x1="70"
              x2="945"
              y1={260 - part * 240}
              y2={260 - part * 240}
              stroke="#e2e8f0"
            />
            <text
              x="60"
              y={265 - part * 240}
              textAnchor="end"
              fill="#64748b"
              fontSize="12"
            >
              {number(high * part)}
            </text>
          </g>
        ))}
        {data.result.map((series, index) => {
          let drawing = false;
          let previous = 0;
          const step =
            series.values.length > 1
              ? series.values[1][0] - series.values[0][0]
              : 0;
          const path = series.values
            .map(([time, raw]) => {
              const val = Number(raw);
              if (!Number.isFinite(val)) {
                drawing = false;
                return "";
              }
              const command =
                drawing && time - previous <= step * 1.5 ? "L" : "M";
              drawing = true;
              previous = time;
              return `${command}${70 + ((time - first) / Math.max(last - first, 1)) * 875},${260 - (val / high) * 240}`;
            })
            .join(" ");
          return (
            <path
              key={series.name}
              d={path}
              stroke={colors[index % colors.length]}
              strokeWidth="2"
              fill="none"
            />
          );
        })}
        <text x="70" y="290" fontSize="12" fill="#64748b">
          {new Date(first * 1000).toLocaleString()}
        </text>
        <text x="945" y="290" textAnchor="end" fontSize="12" fill="#64748b">
          {new Date(last * 1000).toLocaleString()}
        </text>
      </svg>
      <div className="monitoring-table-scroll">
        <table>
          <thead>
            <tr>
              <th>序列</th>
              <th>最新采样 ({unit})</th>
              <th>采样时间</th>
            </tr>
          </thead>
          <tbody>
            {data.result.map((series, index) => {
              const latest = series.values.at(-1);
              return (
                <tr key={series.name}>
                  <th scope="row">
                    <span style={{ color: colors[index % colors.length] }}>
                      ●{" "}
                    </span>
                    {series.name}
                  </th>
                  <td>{number(latest ? Number(latest[1]) : undefined)}</td>
                  <td>
                    {latest ? new Date(latest[0] * 1000).toLocaleString() : "—"}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </>
  );
}
