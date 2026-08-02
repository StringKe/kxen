export default function NumberField(props: {
  label: string;
  value: string;
  set: (value: string) => void;
  disabled?: boolean;
}) {
  return (
    <label class="text-2xs text-[var(--text-faint)]">
      {props.label}
      <input
        type="number"
        min="0"
        step="any"
        class="mt-1 w-full bg-transparent border border-[var(--border)] rounded px-2 py-1 text-xs"
        value={props.value}
        disabled={props.disabled}
        onInput={(event) => props.set(event.currentTarget.value)}
      />
    </label>
  );
}
