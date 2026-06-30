module sink #(parameter int W) (
    input  logic [W-1:0] x,
    output logic [7:0] result
);
    logic [7:0] __block_0;
    assign __block_0 = type(__block_0)'(x);
    assign result = __block_0;
endmodule

module top #(parameter int N) (
    input  logic [7:0] x,
    output logic [7:0] result
);
    function automatic int sum_to(input int n);
        int acc;
        acc = 0;
        for (int i = 0; i < n; i++) begin
            acc = (acc + i);
        end
        return acc;
    endfunction
    localparam int w = sum_to(N);
    logic [w-1:0] wide;
    logic [w-1:0] __block_0;
    assign __block_0 = type(__block_0)'(x);
    assign wide = __block_0;
    sink #(
        .W(w)
    ) sink (
        .x(wide),
        .result(result)
    );
endmodule
