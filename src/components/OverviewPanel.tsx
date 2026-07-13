import {
  ArrowRightIcon,
  ExclamationTriangleIcon,
  LightningBoltIcon,
  PlusIcon,
} from "@radix-ui/react-icons";
import type { AppListItem, AppRunSnapshot, ServiceStatus } from "../types";
import { aggregateStatus, StatusDot } from "./StatusDot";

type OverviewPanelProps = {
  items: AppListItem[];
  live: Record<string, AppRunSnapshot>;
  onOpenApp: (name: string) => void;
  onAddProject: () => void;
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
  onOpenApp,
  onAddProject,
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
          eyebrow: "Projects running",
          title: "All running services are healthy.",
          copy: `${plural(runningProjects, "project")} and ${plural(liveServices, "service")} are running on this Mac.`,
        }
      : items.length
        ? {
            state: "idle",
            eyebrow: "No projects running",
            title: "Projects are stopped.",
            copy: "Start a project or review the servers already running on this Mac.",
          }
        : {
            state: "empty",
            eyebrow: "Get started",
            title: "Add your first project.",
            copy: "Choose a project folder or review servers already running on this Mac.",
          };

  return (
    <section className="overview-page" aria-labelledby="overview-title">
      <header className="page-header overview-header">
        <div className="page-header-copy">
          <h1 className="page-title" id="overview-title">
            Overview
          </h1>
          <p className="page-description">
            Projects, services, and connections.
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
          </div>
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
            <dt>Running services</dt>
            <dd className="overview-metric-value">{liveServices}</dd>
            <dd className="overview-metric-detail">on this Mac</dd>
          </div>
          <div
            className="overview-metric"
            data-tone={attentionCount ? "attention" : "clear"}
          >
            <dt>Needs attention</dt>
            <dd className="overview-metric-value">{attentionCount}</dd>
            <dd className="overview-metric-detail">
              {attentionCount ? "review or repair" : "none"}
            </dd>
          </div>
        </dl>

        {projects.length > 0 && (
          <section
            className="overview-projects"
            aria-labelledby="overview-projects-title"
          >
            <div className="overview-section-heading">
              <div>
                <h2 id="overview-projects-title">Projects</h2>
              </div>
              <span>{plural(items.length, "project")}</span>
            </div>

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
          </section>
        )}
      </div>
    </section>
  );
}
