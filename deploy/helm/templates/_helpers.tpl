{{/*
Expand the name of the chart.
*/}}
{{- define "auto-review.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Fully qualified name (release-name + chart name).
*/}}
{{- define "auto-review.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name (include "auto-review.name" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{/*
Common labels.
*/}}
{{- define "auto-review.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
app.kubernetes.io/name: {{ include "auto-review.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{/*
Selector labels.
*/}}
{{- define "auto-review.selectorLabels" -}}
app.kubernetes.io/name: {{ include "auto-review.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/*
Name of the secret carrying tokens. Either user-provided via
secretRef or one we create.
*/}}
{{- define "auto-review.secretName" -}}
{{- if .Values.secrets.secretRef -}}
{{- .Values.secrets.secretRef -}}
{{- else -}}
{{- include "auto-review.fullname" . -}}
{{- end -}}
{{- end -}}
