import { useState } from "react";
import { Activity, Clock3, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  charts,
  formatMetric as number,
  projectSnapshot,
  type Chart,
} from "./metrics";
import { useMonitoringSnapshot, useMonitoringHistory } from "./queries";
import { HistoryChart } from "./HistoryChart";
import "./monitoring.css";

const emptyView = projectSnapshot();

/** Presents local and historical monitoring without implementing metric or transport policy. */
export function MonitoringPage() {
  const [mode, setMode] = useState<"live" | "history">("live");
  const [chart, setChart] = useState<Chart>("backlog");
  const [seconds, setSeconds] = useState(3600);
  const live = useMonitoringSnapshot();
  const history = useMonitoringHistory(chart, seconds, mode === "history");
  const view = live.data ?? emptyView;
  const { process, hasWorker, dbStale, queues, models } = view;
  return (
    <main className="monitoring-page" id="main-content">
      <div className="monitoring-heading">
        <div>
          <p className="monitoring-eyebrow">SERVICE OBSERVABILITY</p>
          <h1>运行监测</h1>
          <p>从任务等待到模型推理，查看吞吐与背压。</p>
        </div>
        <Activity size={32} aria-hidden="true" />
      </div>
      <div className="monitoring-toolbar">
        <div role="group" aria-label="查看方式">
          <Button
            variant={mode === "live" ? "default" : "outline"}
            onClick={() => setMode("live")}
          >
            实时指标
          </Button>
          <Button
            variant={mode === "history" ? "default" : "outline"}
            onClick={() => setMode("history")}
          >
            历史指标
          </Button>
        </div>
        <span>
          <Clock3 size={15} aria-hidden="true" />
          {view.collectedAt !== undefined
            ? `采集于 ${new Date(view.collectedAt! * 1000).toLocaleTimeString()}`
            : "等待首次采集"}{" "}
          · 每 5 秒刷新
        </span>
      </div>
      <p className="monitoring-scope" role="status">
        {mode === "live"
          ? `实时范围：当前进程 ${process?.id ?? "未知"} · 角色 ${process?.role ?? "未知"}。任务队列统计来自共享数据库，资源与吞吐仅属于当前进程。`
          : "历史范围：Prometheus 中 service=docparse 的采集实例。"}
      </p>
      {mode === "live" && !hasWorker && (
        <p className="monitoring-alert" role="status">
          {process?.role === "api"
            ? "当前为 API-only 实例，不执行模型推理。此页不将 worker 吞吐、PDFium 或模型资源显示为零；请切换历史指标查看已采集的 worker 数据。"
            : "进程角色尚未确认，暂不展示推理资源和吞吐。"}
        </p>
      )}
      {live.isError && (
        <p className="monitoring-alert" role="alert">
          实时指标暂时不可用。下方保留上次数据，可能已过期。
        </p>
      )}
      {mode === "live" ? (
        <>
          {dbStale && (
            <p className="monitoring-alert" role="status">
              数据库统计尚未更新或采集失败；任务数与最老等待时间可能已过期。
            </p>
          )}
          <section className="monitoring-cards" aria-label="关键指标">
            {view.cards.map((card) => (
              <article className="monitoring-card" key={card.label}>
                <span>{card.label}</span>
                <strong>{number(card.value)}</strong>
                <small>{card.unit}</small>
              </article>
            ))}
          </section>
          {hasWorker && (
            <>
              <section className="monitoring-panel">
                <h2>模型队列与背压</h2>
                <p>
                  队列中等待的输入不包括正在推理的批次。提交者阻塞代表正在等待空位。
                </p>
                <div className="monitoring-table-scroll">
                  <table>
                    <thead>
                      <tr>
                        <th>队列</th>
                        <th>占用 / 容量</th>
                        <th>占用率</th>
                        <th>阻塞提交者</th>
                        <th>入队数量（累计）</th>
                      </tr>
                    </thead>
                    <tbody>
                      {queues.map((queue) => (
                        <tr key={queue.name}>
                          <th scope="row">{queue.name}</th>
                          <td>
                            {number(queue.used)} / {number(queue.capacity)}
                          </td>
                          <td>
                            <meter
                              aria-label={`${queue.name} 占用率`}
                              min={0}
                              max={queue.capacity || 1}
                              value={queue.used ?? 0}
                            />{" "}
                            {number(queue.percent)}%
                          </td>
                          <td>{number(queue.blocked)}</td>
                          <td>{number(queue.enqueued)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
                {!queues.length && <p>尚无模型队列数据。</p>}
              </section>
              <section className="monitoring-panel">
                <h2>模型消费者</h2>
                <div className="monitoring-table-scroll">
                  <table>
                    <thead>
                      <tr>
                        <th>模型</th>
                        <th>存活 / 配置</th>
                        <th>忙碌</th>
                        <th>批次上限</th>
                      </tr>
                    </thead>
                    <tbody>
                      {models.map((model) => (
                        <tr key={model.name}>
                          <th scope="row">{model.name}</th>
                          <td>
                            {number(model.alive)} / {number(model.configured)}
                          </td>
                          <td>{number(model.busy)}</td>
                          <td>{number(model.batchLimit)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </section>
              <section className="monitoring-panel">
                <h2>实际推理性能</h2>
                <p>
                  当前进程启动以来的成功调用均值；编码器与每次解码器调用分别统计。P95
                  和吞吐趋势见历史指标。
                </p>
                <div className="monitoring-table-scroll">
                  <table>
                    <thead>
                      <tr>
                        <th>模型 / 图</th>
                        <th>成功调用</th>
                        <th>平均耗时（毫秒）</th>
                        <th>平均批次</th>
                        <th>失败调用</th>
                      </tr>
                    </thead>
                    <tbody>
                      {view.inference.map((row) => (
                        <tr key={`${row.model}/${row.graph}`}>
                          <th scope="row">
                            {row.model} / {row.graph}
                          </th>
                          <td>{number(row.successes)}</td>
                          <td>{number(row.averageMs)}</td>
                          <td>{number(row.averageBatch)}</td>
                          <td>{number(row.failures)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </section>
            </>
          )}
        </>
      ) : (
        <section className="monitoring-panel">
          <div className="monitoring-history-controls">
            <label>
              指标
              <select
                aria-label="指标"
                value={chart}
                onChange={(event) =>
                  setChart(event.target.value as keyof typeof charts)
                }
              >
                {Object.entries(charts).map(([key, [label]]) => (
                  <option key={key} value={key}>
                    {label}
                  </option>
                ))}
              </select>
            </label>
            <label>
              时间范围
              <select
                aria-label="时间范围"
                value={seconds}
                onChange={(event) => setSeconds(Number(event.target.value))}
              >
                {[
                  [900, "最近 15 分钟"],
                  [3600, "最近 1 小时"],
                  [21600, "最近 6 小时"],
                  [86400, "最近 24 小时"],
                  [604800, "最近 7 天"],
                ].map(([val, label]) => (
                  <option key={val} value={val}>
                    {label}
                  </option>
                ))}
              </select>
            </label>
            <Button
              variant="outline"
              onClick={() => void history.refetch()}
              disabled={history.isFetching}
            >
              <RefreshCw size={16} aria-hidden="true" />
              刷新
            </Button>
          </div>
          <p>
            Prometheus 历史 · 每 30 秒刷新 · 无采样和无请求的分位数显示为空缺。
          </p>
          {history.isError && (
            <p className="monitoring-alert" role="alert">
              历史指标暂时不可用。请检查 Prometheus
              配置、连接与采集目标；已有数据保留但可能过期。
            </p>
          )}
          {view.historyAvailable === false && (
            <p role="status">服务尚未配置 Prometheus 历史查询地址。</p>
          )}
          {history.isPending && <p role="status">正在读取历史指标…</p>}
          {history.data && (
            <HistoryChart
              data={history.data}
              title={charts[chart][0]}
              unit={charts[chart][1]}
            />
          )}
        </section>
      )}
    </main>
  );
}
