using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Collections.Specialized;
using System.ComponentModel;

namespace Looker.ViewModels;

/// <summary>
/// An <see cref="ObservableCollection{T}"/> that can be re-filled in one shot. A folder open or sort
/// change replaces the whole list; raising thousands of individual Add notifications there would
/// stall the bound ListView, so <see cref="Reset"/> collapses it into a single Reset. Incremental
/// live edits still use the normal <see cref="ObservableCollection{T}.Insert"/>/<see cref="ObservableCollection{T}.RemoveAt"/>.
/// </summary>
public sealed class RangeObservableCollection<T> : ObservableCollection<T>
{
    public void Reset(IEnumerable<T> items)
    {
        Items.Clear();
        foreach (T item in items)
            Items.Add(item);

        OnPropertyChanged(new PropertyChangedEventArgs(nameof(Count)));
        OnPropertyChanged(new PropertyChangedEventArgs("Item[]"));
        OnCollectionChanged(new NotifyCollectionChangedEventArgs(NotifyCollectionChangedAction.Reset));
    }
}
