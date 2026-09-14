namespace Looker.Navigation;

public enum SortField
{
    Name,
    DateModified,
    Size,
}

public enum SortDirection
{
    Ascending,
    Descending,
}

/// <summary>How the current folder's files are ordered. Persisted across sessions (M8).</summary>
public readonly record struct SortMode(SortField Field, SortDirection Direction)
{
    public static readonly SortMode Default = new(SortField.Name, SortDirection.Ascending);
}
