import {
  ArrowRightIcon,
  CheckCircledIcon,
  ExclamationTriangleIcon,
  GearIcon,
  GlobeIcon,
  LightningBoltIcon,
  PlusIcon,
} from "@radix-ui/react-icons";
import type {
  AgentStatus,
  AppListItem,
  AppRunSnapshot,
  ServiceStatus,
} from "../types";
import { aggregateStatus, StatusDot } from "./StatusDot";

type OverviewPanelProps = {
  items: AppListItem[];
  live: Record<string, AppRunSnapshot>;
  agents: AgentStatus | null;
  onOpenApp: (name: string) => void;
  onAddProject: () => void;
  onOpenServers: () => void;
  onOpenConnections: () => void;
};

const STATUS_LABEL: Record<ServiceStatus, string> = {
  stopped: "Stopped",
  starting: "Starting",
  ready: "Running",
  unhealthy: "Needs attention",
  exited: "Exited",
};

function plural(count: number, singular: string, pluralForm = `${singular}s`) {
  return `${count} ${count === 1 ? singular : pluralForm}`;
}

export function OverviewPanel({
  items,
  live,
  agents,
  onOpenApp,
  onAddProject,
  onOpenServers,
  onOpenConnections,
}: OverviewPanelProps) {
  const projects = items.map((item) => {
    const run = live[item.config.name] ?? item.run;
    const status = aggregateStatus(run);
    const running = run?.running ?? item.running;
    const liveServices =
      run?.services.filter(
        (service) =>
          service.status !== "stopped" && service.status !== "exited",
      ).length ?? 0;
    const readyServices =
      run?.services.filter((service) => service.status === "ready").length ?? 0;
    const port = run?.services.find((service) => service.port != null)?.port;
    const needsReview = item.config.trusted === false;
    const hasRuntimeProblem =
      run?.services.some(
        (service) =>
          service.status === "unhealthy" ||
          (service.status === "exited" &&
            service.exitCode != null &&
            service.exitCode !== 0),
      ) ?? false;
    const needsAttention =
      needsReview || status === "unhealthy" || hasRuntimeProblem;

    return {
      item,
      run,
      status,
      running,
      liveServices,
      readyServices,
      port,
      needsReview,
      needsAttention,
    };
  });

  const runningProjects = projects.filter((project) => project.running).length;
  const liveServices = projects.reduce(
    (total, project) => total + project.liveServices,
    0,
  );
  const attentionCount = projects.filter(
    (project) => project.needsAttention,
  ).length;
  const connectedAgents = agents
    ? [
        agents.codeConnected,
        agents.desktopConnected,
        agents.codexConnected,
      ].filter(Boolean).length
    : 0;

  const hero = attentionCount
    ? {
        state: "attention",
        eyebrow: "Attention requested",
        title:
          attentionCount === 1
            ? "1 project needs attention."
            : `${attentionCount} projects need attention.`,
        copy: "Review the highlighted projects before the next run. Harbor will keep unapproved commands safely paused.",
      }
    : runningProjects
      ? {
          state: "active",
          eyebrow: "Local stack online",
          title: "Everything is running smoothly.",
          copy: `${plural(runningProjects, "project")} and ${plural(liveServices, "service")} are live on this Mac.`,
        }
      : items.length
        ? {
            state: "idle",
            eyebrow: "All clear",
            title: "Your harbor is quiet.",
            copy: "Your projects are ready when you are. Start one from its project page or inspect what is already listening locally.",
          }
        : {
            state: "empty",
            eyebrow: "Welcome to Harbor",
            title: "Bring your local stack into view.",
            copy: "Add a project or inspect the servers already running on this Mac. Harbor will map the pieces without taking over your workflow.",
          };

  return (
    <section className="overview-page" aria-labelledby="overview-title">
      <header className="page-header overview-header">
        <div className="page-header-copy">
          <div className="page-eyebrow">Control deck</div>
          <h1 className="page-title" id="overview-title">
            Overview
          </h1>
          <p className="page-description">
            Projects, local services, and AI connections at a glance.
          </p>
        </div>
        <button
          type="button"
          className="overview-header-action"
          onClick={onAddProject}
          aria-label="Add a project to Harbor"
        >
          <PlusIcon aria-hidden="true" />
          Add project
        </button>
      </header>

      <div className="overview-scroll">
        <section
          className="overview-hero"
          data-state={hero.state}
          aria-labelledby="overview-hero-title"
        >
          <div className="overview-hero-main">
            <div className="overview-hero-eyebrow">
              {attentionCount ? (
                <ExclamationTriangleIcon aria-hidden="true" />
              ) : (
                <LightningBoltIcon aria-hidden="true" />
              )}
              {hero.eyebrow}
            </div>
            <h2 className="overview-hero-title" id="overview-hero-title">
              {hero.title}
            </h2>
            <p className="overview-hero-copy">{hero.copy}</p>

            <div className="overview-quick-actions" aria-label="Quick actions">
              <button
                type="button"
                className="overview-quick-action overview-quick-action-primary"
                onClick={onAddProject}
                aria-label="Add a new project to Harbor"
              >
                <PlusIcon aria-hidden="true" />
                <span>
                  <strong>Add a project</strong>
                  <small>Scan a folder and create its services</small>
                </span>
                <ArrowRightIcon aria-hidden="true" />
              </button>
              <button
                type="button"
                className="overview-quick-action"
                onClick={onOpenServers}
                aria-label="Inspect local servers running on this Mac"
              >
                <GlobeIcon aria-hidden="true" />
                <span>
                  <strong>Local servers</strong>
                  <small>See every listener and duplicate</small>
                </span>
                <ArrowRightIcon aria-hidden="true" />
              </button>
            </div>
          </div>

          <button
            type="button"
            className="overview-connection-card"
            data-connected={connectedAgents > 0 || undefined}
            onClick={onOpenConnections}
            aria-label={
              connectedAgents
                ? `Manage ${plural(connectedAgents, "connected AI client")}`
                : "Connect an AI client to Harbor"
            }
          >
            <span className="overview-connection-icon" aria-hidden="true">
              {connectedAgents ? <CheckCircledIcon /> : <GearIcon />}
            </span>
            <span className="overview-connection-copy">
              <small>AI connections</small>
              <strong>
                {agents === null
                  ? "Checking connections…"
                  : connectedAgents
                    ? plural(connectedAgents, "client connected")
                    : "Ready to connect"}
              </strong>
            </span>
            <ArrowRightIcon aria-hidden="true" />
          </button>
        </section>

        <dl className="overview-metrics" aria-label="Harbor activity summary">
          <div className="overview-metric" data-tone="active">
            <dt>Running projects</dt>
            <dd className="overview-metric-value">{runningProjects}</dd>
            <dd className="overview-metric-detail">
              of {items.length} registered
            </dd>
          </div>
          <div className="overview-metric" data-tone="service">
            <dt>Live services</dt>
            <dd className="overview-metric-value">{liveServices}</dd>
            <dd className="overview-metric-detail">across this Mac</dd>
          </div>
          <div
            className="overview-metric"
            data-tone={attentionCount ? "attention" : "clear"}
          >
            <dt>Needs attention</dt>
            <dd className="overview-metric-value">{attentionCount}</dd>
            <dd className="overview-metric-detail">
              {attentionCount ? "review or repair" : "all systems clear"}
            </dd>
          </div>
        </dl>

        <section
          className="overview-projects"
          aria-labelledby="overview-projects-title"
        >
          <div className="overview-section-heading">
            <div>
              <div className="page-eyebrow">Workspace</div>
              <h2 id="overview-projects-title">Projects</h2>
            </div>
            <span>{plural(items.length, "project")}</span>
          </div>

          {projects.length ? (
            <ul className="overview-project-grid">
              {projects.map(
                ({
                  item,
                  run,
                  status,
                  liveServices: projectLiveServices,
                  readyServices,
                  port,
                  needsReview,
                  needsAttention,
                }) => {
                  const serviceCount = item.config.services.length;
                  const statusLabel = needsReview
                    ? "Review required"
                    : needsAttention
                      ? "Needs attention"
                      : STATUS_LABEL[status];

                  return (
                    <li
                      className="overview-project-item"
                      key={item.config.name}
                    >
                      <button
                        type="button"
                        className="overview-project-card"
                        data-status={status}
                        data-attention={needsAttention || undefined}
                        onClick={() => onOpenApp(item.config.name)}
                        aria-label={`Open ${item.config.name}, ${statusLabel}`}
                      >
                        <span className="overview-project-card-top">
                          <span className="overview-project-identity">
                            <span
                              className="overview-project-mark"
                              aria-hidden="true"
                            >
                              {item.config.name.slice(0, 1).toUpperCase()}
                            </span>
                            <span className="overview-project-name">
                              <strong>{item.config.name}</strong>
                              <small>{item.config.root}</small>
                            </span>
                          </span>
                          <ArrowRightIcon
                            className="overview-project-arrow"
                            aria-hidden="true"
                          />
                        </span>

                        <span className="overview-project-status">
                          <StatusDot status={status} />
                          <span>{statusLabel}</span>
                          {port != null && (
                            <code className="overview-project-port">
                              :{port}
                            </code>
                          )}
                        </span>

                        <span className="overview-project-footer">
                          <span>{plural(serviceCount, "service")}</span>
                          {run?.running && (
                            <span>
                              {readyServices} ready · {projectLiveServices} live
                            </span>
                          )}
                          {needsReview && (
                            <span className="overview-project-review">
                              <ExclamationTriangleIcon aria-hidden="true" />
                              Approval needed
                            </span>
                          )}
                        </span>
                      </button>
                    </li>
                  );
                },
              )}
            </ul>
          ) : (
            <div className="overview-project-empty">
              <span className="overview-project-empty-icon" aria-hidden="true">
                <PlusIcon />
              </span>
              <h3>Your projects will dock here.</h3>
              <p>
                Add a folder for Harbor to detect its services, commands, and
                ports automatically.
              </p>
              <div className="overview-project-empty-actions">
                <button type="button" onClick={onAddProject}>
                  <PlusIcon aria-hidden="true" /> Add project
                </button>
                <button type="button" onClick={onOpenServers}>
                  <GlobeIcon aria-hidden="true" /> Inspect local servers
                </button>
              </div>
            </div>
          )}
        </section>
      </div>
    </section>
  );
}
