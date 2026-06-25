module fill #(parameter int N) (
    input  logic [7:0] seed,
    output logic [7:0] result
);
    function automatic int double(input int n);
        int acc;
        acc = 0;
        for (int i = 0; i < n; i++) begin
            acc = (acc + 2);
        end
        return acc;
    endfunction
    localparam int w = double(N);
    logic [7:0] cells [0:w-1];
    for (genvar i = 0; i < w; i++) begin : g_i
        assign cells[i] = seed;
    end
    assign result = cells[0];
endmodule
